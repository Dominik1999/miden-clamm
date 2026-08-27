//! Host-side wrapper for the hand-written AMM math MASM library.
//!
//! The MASM sources live in `asm/` and are assembled into a [`Library`] under the
//! `amm::math` namespace with the transaction-kernel assembler, exactly like the
//! standard protocol components (`miden-standards/build.rs`). The invocation helpers
//! execute small driver programs directly on the VM processor with the core library's
//! event handlers registered — the same handlers every transaction host (including the
//! ntx-builder's executor) registers, so the advice-backed divisions behave identically
//! to production execution.

use std::path::Path;
use std::sync::Arc;

use miden_assembly::Library;
use miden_core_lib::CoreLibrary;
use miden_processor::advice::AdviceInputs;
use miden_processor::{
    DefaultHost, ExecutionError, ExecutionOptions, FastProcessor, Program, StackInputs,
};
use miden_protocol::transaction::TransactionKernel;

/// Namespace the MASM library is assembled under.
pub const LIBRARY_NAMESPACE: &str = "amm::math";

/// Assembles the `asm/` directory into the `amm::math` library.
pub fn assemble_library() -> Library {
    let asm_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("asm");
    let assembler = TransactionKernel::assembler().with_warnings_as_errors(true);
    let lib = assembler
        .assemble_library_from_dir(asm_dir, LIBRARY_NAMESPACE)
        .unwrap_or_else(|err| panic!("amm-math-masm MASM library must assemble: {err}"));
    Arc::unwrap_or_clone(lib)
}

/// Assembles a driver program (plain `begin .. end` source) against the given library.
pub fn assemble_program(lib: &Library, source: &str) -> Program {
    let mut assembler = TransactionKernel::assembler();
    assembler
        .link_dynamic_library(lib.clone())
        .expect("amm::math library must link");
    assembler
        .assemble_program(source)
        .unwrap_or_else(|err| panic!("driver program must assemble: {err}\nsource:\n{source}"))
}

/// Builds a host with the core library (MAST forest + advice event handlers) and the
/// `amm::math` library loaded.
fn build_host(lib: &Library) -> DefaultHost {
    let core_lib = CoreLibrary::default();
    let mut host = DefaultHost::default();
    host.load_library(&core_lib).expect("core library must load");
    host.load_library(lib.mast_forest().clone())
        .expect("amm::math library must load");
    host
}

/// Executes a driver program with the given advice-stack inputs.
///
/// Advice values are consumed FIFO: `advice_stack[0]` is popped by the first
/// `adv_push` the program executes.
///
/// Returns the full 16-element operand stack output (top first).
pub fn execute(
    lib: &Library,
    program: &Program,
    advice_stack: &[u64],
) -> Result<Vec<u64>, ExecutionError> {
    let mut host = build_host(lib);
    let advice = advice_inputs_for_adv_push(advice_stack);
    let output = miden_processor::execute_sync(
        program,
        StackInputs::default(),
        advice,
        &mut host,
        ExecutionOptions::default(),
    )?;
    Ok(stack_to_u64(&output.stack))
}

/// Executes a driver program and returns `(stack_outputs, main_trace_len)`.
///
/// `main_trace_len` is the number of VM clock cycles (main trace rows before padding),
/// the same metric MockChain's `measurements().total_cycles()` reports per segment.
pub fn execute_with_cycles(
    lib: &Library,
    program: &Program,
    advice_stack: &[u64],
) -> Result<(Vec<u64>, usize), ExecutionError> {
    let mut host = build_host(lib);
    let advice = advice_inputs_for_adv_push(advice_stack);
    let processor =
        FastProcessor::new_with_options(StackInputs::default(), advice, ExecutionOptions::default())
            .expect("processor construction must succeed");
    let trace_inputs = processor.execute_trace_inputs_sync(program, &mut host)?;
    let trace = miden_processor::trace::build_trace(trace_inputs)
        .expect("trace generation must succeed for a successful execution");
    let cycles = trace.trace_len_summary().main_trace_len();
    let stack = stack_to_u64(trace.stack_outputs());
    Ok((stack, cycles))
}

fn advice_inputs_for_adv_push(values: &[u64]) -> AdviceInputs {
    let mut builder = miden_processor::advice::AdviceStackBuilder::new();
    // FIFO semantics: values[0] is consumed by the first `adv_push`. All drivers in
    // this crate store each advice value to memory as it is popped, so the first
    // value lands at the lowest limb address.
    builder.push_u64_slice(values);
    builder.build()
}

fn stack_to_u64(stack: &miden_processor::StackOutputs) -> Vec<u64> {
    stack.iter().map(|f| f.as_canonical_u64()).collect()
}

// VALUE ENCODING HELPERS
// ================================================================================================

/// Splits a u128 into 4 little-endian u32 limbs (limb 0 least significant).
pub fn u128_to_limbs(x: u128) -> [u64; 4] {
    [
        (x & 0xFFFF_FFFF) as u64,
        ((x >> 32) & 0xFFFF_FFFF) as u64,
        ((x >> 64) & 0xFFFF_FFFF) as u64,
        ((x >> 96) & 0xFFFF_FFFF) as u64,
    ]
}

/// Reassembles 4 little-endian u32 limbs into a u128.
pub fn limbs_to_u128(limbs: &[u64]) -> u128 {
    assert!(limbs.len() >= 4);
    let mut x = 0u128;
    for (i, &l) in limbs.iter().take(4).enumerate() {
        assert!(l <= 0xFFFF_FFFF, "limb {i} out of u32 range: {l}");
        x |= (l as u128) << (32 * i);
    }
    x
}
