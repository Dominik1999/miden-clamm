use std::{path::Path, sync::Arc};

use anyhow::Context;
use integration::helpers::build_project_in_dir;
use miden_client::{
    account::{
        component::{InitStorageData, StorageValueName},
        AccountBuilder, AccountComponent, AccountType, StorageSlotName,
    },
    auth::AuthSchemeId,
    crypto::RandomCoin,
    note::NoteScript,
    transaction::RawOutputNote,
    Felt, Word,
};
use miden_standards::testing::note::NoteBuilder;
use miden_testing::{AccountState, Auth, MockChain};

/// ntx-builder per-network-tx cycle budget (CLI default, DESIGN.md Part 1f).
const NTX_CYCLE_BUDGET: usize = 1 << 18; // 262,144

/// DESIGN.md Part 3 item 1: if the reverse tick mapping alone exceeds ~10% of
/// a ~100k per-swap budget, switch to the log2-based algorithm.
const REVERSE_MAPPING_FLAG_THRESHOLD: usize = 10_000;

/// Phase 2 in-VM cycle microbenchmarks of the amm-math crate.
///
/// Each benchmark consumes one note (in its own transaction) that calls a
/// math-bench component procedure running one amm-math primitive on
/// hardcoded inputs. Selector 0 is a no-op procedure establishing the
/// baseline transaction overhead; every primitive is reported raw and net
/// (measured - baseline). Cycle counts are deterministic, so each bench
/// runs once.
#[tokio::test]
async fn math_bench_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();

    let sender = builder.add_existing_wallet(Auth::BasicAuth {
        auth_scheme: AuthSchemeId::Falcon512Poseidon2,
    })?;

    // Build contracts
    let contract_package = Arc::new(build_project_in_dir(
        Path::new("../contracts/math-bench"),
        true,
    )?);
    let note_package = Arc::new(build_project_in_dir(
        Path::new("../contracts/bench-note"),
        true,
    )?);

    // Seed the (unused) scratch value slot so the component schema is satisfied.
    let scratch_slot = StorageSlotName::new("math_bench::math_bench::scratch")
        .context("invalid math-bench storage slot name")?;
    let mut init_storage_data = InitStorageData::default();
    init_storage_data.insert_value(
        StorageValueName::from_slot_name(&scratch_slot),
        Word::default(),
    )?;

    let bench_component = AccountComponent::from_package(&contract_package, &init_storage_data)
        .context("failed to build account component from math-bench package")?;
    let bench_account = builder.add_account_from_builder(
        Auth::BasicAuth {
            auth_scheme: AuthSchemeId::Falcon512Poseidon2,
        },
        AccountBuilder::new([7_u8; 32])
            .account_type(AccountType::Public)
            .with_component(bench_component),
        AccountState::Exists,
    )?;

    // (selector, label) -- selector 0 is the no-op baseline.
    let benches: [(u32, &str); 7] = [
        (0, "baseline_noop"),
        (1, "mul_div_floor"),
        (2, "get_sqrt_ratio_at_tick(12345)"),
        (3, "get_sqrt_ratio_at_tick(+443636)"),
        (4, "get_sqrt_ratio_at_tick(-443636)"),
        (5, "get_tick_at_sqrt_ratio"),
        (6, "compute_swap_step"),
    ];

    // One note per benchmark, all seeded on the chain up front.
    let mut note_rng = RandomCoin::new(Word::from(
        NoteScript::from_package(note_package.as_ref())
            .context("failed to build note script from package")?
            .root(),
    ));
    let mut notes = Vec::new();
    for (selector, _) in &benches {
        let note = NoteBuilder::new(sender.id(), &mut note_rng)
            .package((*note_package).clone())
            .note_storage([Felt::from(*selector)])?
            .build()
            .context("failed to build bench note from package")?;
        builder.add_output_note(RawOutputNote::Full(note.clone()));
        notes.push(note);
    }

    let mut mock_chain = builder.build()?;

    // Consume each note in its own transaction and record measurements.
    struct Row {
        label: &'static str,
        note_cycles: usize,
        total_cycles: usize,
        prologue: usize,
        notes_processing: usize,
        epilogue: usize,
        auth_procedure: usize,
    }
    let mut rows: Vec<Row> = Vec::new();

    for ((_, label), note) in benches.iter().zip(&notes) {
        let tx_context = mock_chain
            .build_tx_context(bench_account.id(), &[note.id()], &[])?
            .build()?;
        let executed = tx_context
            .execute()
            .await
            .with_context(|| format!("bench transaction failed for {label}"))?;

        let m = executed.measurements();
        // Each bench tx consumes exactly one note; take its measurement.
        // (The kernel-side NoteId in `note_execution` is not directly
        // comparable to the host-side `note.id()` encoding in v0.15.)
        anyhow::ensure!(
            m.note_execution.len() == 1,
            "expected exactly one note execution measurement, got {:?}",
            m.note_execution
        );
        let note_cycles = m.note_execution[0].1;
        rows.push(Row {
            label,
            note_cycles,
            total_cycles: m.total_cycles(),
            prologue: m.prologue,
            notes_processing: m.notes_processing,
            epilogue: m.epilogue,
            auth_procedure: m.auth_procedure,
        });

        mock_chain.add_pending_executed_transaction(&executed)?;
        mock_chain.prove_next_block()?;
    }

    // Report.
    let baseline = &rows[0];
    let baseline_note = baseline.note_cycles;
    let baseline_total = baseline.total_cycles;

    println!();
    println!("== amm-math in-VM cycle microbenchmarks (MockChain, release) ==");
    println!(
        "budget: 2^18 = {NTX_CYCLE_BUDGET} cycles per network tx (ntx-builder CLI default)"
    );
    println!();
    println!(
        "{:<34} {:>12} {:>12} {:>12} {:>12}",
        "primitive", "note cycles", "net note", "total tx", "net total"
    );
    for row in &rows {
        println!(
            "{:<34} {:>12} {:>12} {:>12} {:>12}",
            row.label,
            row.note_cycles,
            row.note_cycles as i64 - baseline_note as i64,
            row.total_cycles,
            row.total_cycles as i64 - baseline_total as i64,
        );
    }
    println!();
    println!(
        "baseline breakdown: prologue={} notes_processing={} epilogue={} (of which auth={}) total={}",
        baseline.prologue,
        baseline.notes_processing,
        baseline.epilogue,
        baseline.auth_procedure,
        baseline.total_cycles
    );
    println!(
        "note: the baseline fixed overhead includes the Falcon512 signature check \
         (~{} cycles); a network account authenticates via allowlists instead, so \
         real ntx overhead is lower.",
        baseline.auth_procedure
    );

    // Implied swaps-per-network-tx. Fixed overhead = everything in the
    // baseline tx except its note execution; marginal cost of one more
    // swap note = baseline per-note overhead + net math cycles.
    let net = |label: &str| -> usize {
        let row = rows.iter().find(|r| r.label == label).unwrap();
        row.note_cycles.saturating_sub(baseline_note)
    };
    let net_swap_step = net("compute_swap_step");
    let net_reverse = net("get_tick_at_sqrt_ratio");
    let fixed_overhead = baseline_total - baseline_note;
    let marginal_swap_only = baseline_note + net_swap_step;
    let marginal_swap_reverse = baseline_note + net_swap_step + net_reverse;
    let swaps_only = (NTX_CYCLE_BUDGET - fixed_overhead) / marginal_swap_only;
    let swaps_with_reverse = (NTX_CYCLE_BUDGET - fixed_overhead) / marginal_swap_reverse;

    println!();
    println!("implied throughput (2^18 budget, fixed overhead = {fixed_overhead} cycles):");
    println!(
        "  swap-step only:              marginal {marginal_swap_only:>8} cycles/note  -> {swaps_only} swaps per network tx (protocol cap: 20 notes)"
    );
    println!(
        "  swap-step + reverse mapping: marginal {marginal_swap_reverse:>8} cycles/note  -> {swaps_with_reverse} swaps per network tx (protocol cap: 20 notes)"
    );

    if net_reverse > REVERSE_MAPPING_FLAG_THRESHOLD {
        println!();
        println!(
            "*** FLAG: get_tick_at_sqrt_ratio net cost {net_reverse} cycles exceeds the \
             ~{REVERSE_MAPPING_FLAG_THRESHOLD}-cycle switch-to-log2 threshold \
             (DESIGN.md Part 3 item 1) ***"
        );
    }

    // Sanity: every primitive costs at least as much as the baseline, and
    // the baseline itself is sane.
    assert!(baseline_note > 0, "baseline note execution measured 0 cycles");
    for row in &rows[1..] {
        assert!(
            row.note_cycles > baseline_note,
            "{} measured {} note cycles, not above the baseline {}",
            row.label,
            row.note_cycles,
            baseline_note
        );
    }

    Ok(())
}
