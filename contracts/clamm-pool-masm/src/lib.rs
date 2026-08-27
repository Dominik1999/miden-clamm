//! Host-side wrapper for the hand-written clamm-pool MASM component and its four
//! production note scripts (Stage 2 of the MASM port).
//!
//! The pool component MASM lives in `asm/pool.masm` and is assembled under the
//! `clamm::pool` namespace with the transaction-kernel assembler, statically linking
//! the Stage-1 `amm::math` library (so the account code is fully self-contained).
//! The note scripts live in `asm/notes/` and are assembled with the miden-standards
//! library statically linked (Path B reclaims through the STANDARD BasicWallet
//! `receive_asset`) and the pool library dynamically linked (Path A `call`s the pool
//! procedures by their MAST roots; the code executes from the pool account).
//!
//! Storage slot names are identical to the Rust-SDK component
//! (`clamm_pool::clamm_pool::<field>`), so the MockChain testbed reads both backends
//! through the same names, and a committed pool state produced by this component is
//! indistinguishable from the Rust pool's for the same operations.

use std::sync::{Arc, OnceLock};

use miden_protocol::account::component::{AccountComponentCode, AccountComponentMetadata};
use miden_protocol::account::{AccountComponent, StorageMap, StorageSlot, StorageSlotName};
use miden_protocol::assembly::{Library, Parse, ParseOptions, Path};
use miden_protocol::note::NoteScript;
use miden_protocol::transaction::TransactionKernel;
use miden_protocol::utils::serde::Serializable;
use miden_protocol::Word;
use miden_standards::StandardsLib;

/// Namespace path of the pool component library.
pub const POOL_LIBRARY_PATH: &str = "clamm::pool";

/// Component name recorded in the component metadata.
pub const COMPONENT_NAME: &str = "clamm::components::clamm_pool_masm";

/// Value slots of the pool component, in declaration order. All are one Word.
pub const VALUE_SLOT_FIELDS: [&str; 10] = [
    "pool_config",
    "pool_params",
    "p2id_root",
    "sqrt_price",
    "pool_state",
    "liquidity",
    "fee_growth_global0_lo",
    "fee_growth_global0_hi",
    "fee_growth_global1_lo",
    "fee_growth_global1_hi",
];

/// Map slots of the pool component.
pub const MAP_SLOT_FIELDS: [&str; 3] = ["ticks", "tick_bitmap", "positions"];

/// The four production note kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolNoteKind {
    Swap,
    Mint,
    Burn,
    Collect,
}

/// Builds a clamm-pool storage slot name (`clamm_pool::clamm_pool::<field>`),
/// identical to the Rust-SDK component's slot names.
pub fn slot_name(field: &str) -> StorageSlotName {
    StorageSlotName::new(format!("clamm_pool::clamm_pool::{field}"))
        .unwrap_or_else(|err| panic!("invalid clamm-pool slot name for field {field}: {err}"))
}

/// Assembles (once) and returns the pool component library (`clamm::pool`), with the
/// Stage-1 `amm::math` library statically linked.
pub fn pool_library() -> &'static Library {
    static POOL_LIB: OnceLock<Library> = OnceLock::new();
    POOL_LIB.get_or_init(|| {
        let math = amm_math_masm::assemble_library();
        let source_manager: Arc<dyn miden_protocol::assembly::SourceManagerSync> =
            Arc::new(miden_protocol::assembly::DefaultSourceManager::default());
        let mut assembler = TransactionKernel::assembler_with_source_manager(source_manager.clone())
            .with_warnings_as_errors(true);
        assembler
            .link_static_library(&math)
            .expect("amm::math library must link statically");
        let mut options = ParseOptions::for_library();
        options.path = Some(Path::new(POOL_LIBRARY_PATH).into());
        let module = include_str!("../asm/pool.masm")
            .parse_with_options(source_manager, options)
            .unwrap_or_else(|err| panic!("pool.masm must parse: {err}"));
        let library = assembler
            .assemble_library([module])
            .unwrap_or_else(|err| panic!("clamm-pool-masm library must assemble: {err}"));
        Arc::unwrap_or_clone(library)
    })
}

/// Returns the serialized size in bytes of the pool component library — the number
/// that must stay below `ACCOUNT_UPDATE_MAX_SIZE` (256 KiB) for the pool to be
/// deployable by transaction (the Rust build's ~600 KB blocker).
pub fn pool_library_size() -> usize {
    pool_library().to_bytes().len()
}

/// Assembles (once) and returns the note script of the given kind.
///
/// The standards library is statically linked so the reclaim path's
/// `basic::add_assets_to_account` (which `call`s the STANDARD `receive_asset` root)
/// is carried by the note's own MAST forest; the pool library is dynamically linked
/// so Path A's `call.pool::*` resolves against the pool account's code at runtime.
pub fn note_script(kind: PoolNoteKind) -> &'static NoteScript {
    static SCRIPTS: OnceLock<[NoteScript; 4]> = OnceLock::new();
    let scripts = SCRIPTS.get_or_init(|| {
        let build = |source: &str, path: &str| -> NoteScript {
            let source_manager: Arc<dyn miden_protocol::assembly::SourceManagerSync> =
                Arc::new(miden_protocol::assembly::DefaultSourceManager::default());
            let mut assembler =
                TransactionKernel::assembler_with_source_manager(source_manager.clone())
                    .with_warnings_as_errors(true);
            assembler
                .link_static_library(StandardsLib::default())
                .expect("standards library must link statically");
            assembler
                .link_dynamic_library(pool_library())
                .expect("pool library must link dynamically");
            let mut options = ParseOptions::for_library();
            options.path = Some(Path::new(path).into());
            let module = source
                .parse_with_options(source_manager, options)
                .unwrap_or_else(|err| panic!("note script {path} must parse: {err}"));
            let library = assembler
                .assemble_library([module])
                .unwrap_or_else(|err| panic!("note script {path} must assemble: {err}"));
            NoteScript::from_library(&library)
                .unwrap_or_else(|err| panic!("note script {path} must expose @note_script: {err}"))
        };
        [
            build(include_str!("../asm/notes/swap.masm"), "clamm_notes::swap"),
            build(include_str!("../asm/notes/mint.masm"), "clamm_notes::mint"),
            build(include_str!("../asm/notes/burn.masm"), "clamm_notes::burn"),
            build(
                include_str!("../asm/notes/collect.masm"),
                "clamm_notes::collect",
            ),
        ]
    });
    match kind {
        PoolNoteKind::Swap => &scripts[0],
        PoolNoteKind::Mint => &scripts[1],
        PoolNoteKind::Burn => &scripts[2],
        PoolNoteKind::Collect => &scripts[3],
    }
}

/// Initial values for the pool's value slots (the same initialization surface the
/// testbed's `InitStorageData` path feeds the Rust component). Unlisted value slots
/// start zeroed; the three map slots start empty.
#[derive(Debug, Clone, Default)]
pub struct PoolInitStorage {
    /// `[token0_suffix, token0_prefix, token1_suffix, token1_prefix]`.
    pub pool_config: Word,
    /// `[fee_pips, tick_spacing, 0, 0]`.
    pub pool_params: Word,
    /// P2ID note script MAST root.
    pub p2id_root: Word,
    /// Initial sqrtPriceX96 as 4 little-endian u32 limbs.
    pub sqrt_price: Word,
    /// `[initial_tick_offset, 1, 0, 0]`.
    pub pool_state: Word,
}

/// Builds the pool [`AccountComponent`] with the given initial storage.
pub fn component(init: &PoolInitStorage) -> AccountComponent {
    let code = AccountComponentCode::from(pool_library().clone());
    let value = |field: &str, word: Word| StorageSlot::with_value(slot_name(field), word);
    let slots = vec![
        value("pool_config", init.pool_config),
        value("pool_params", init.pool_params),
        value("p2id_root", init.p2id_root),
        value("sqrt_price", init.sqrt_price),
        value("pool_state", init.pool_state),
        value("liquidity", Word::default()),
        value("fee_growth_global0_lo", Word::default()),
        value("fee_growth_global0_hi", Word::default()),
        value("fee_growth_global1_lo", Word::default()),
        value("fee_growth_global1_hi", Word::default()),
        StorageSlot::with_map(slot_name("ticks"), StorageMap::default()),
        StorageSlot::with_map(slot_name("tick_bitmap"), StorageMap::default()),
        StorageSlot::with_map(slot_name("positions"), StorageMap::default()),
    ];
    let metadata = AccountComponentMetadata::new(COMPONENT_NAME)
        .with_description("Hand-written MASM concentrated-liquidity pool (kernel-read, no-args)");
    AccountComponent::new(code, slots, metadata)
        .expect("clamm-pool-masm component must satisfy account component requirements")
}
