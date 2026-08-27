//! Assembly-level checks for the clamm-pool MASM port: the component library and all
//! four note scripts assemble, the serialized component stays below the 256 KiB
//! tx-deployability bound, and the in-VM Poseidon2 `hash_elements` matches the
//! host-side hasher for the exact preimage shapes the pool uses (position keys and
//! P2ID serial derivation, both 5-element preimages).

use clamm_pool_masm::{
    component, note_script, pool_library, pool_library_size, PoolInitStorage, PoolNoteKind,
};
use miden_protocol::{Felt, Hasher, Word};

/// `ACCOUNT_UPDATE_MAX_SIZE`: the serialized account-delta cap that blocked
/// tx-deployment of the ~600 KB Rust pool (DESIGN Part 5, Phase 4 finding 1).
const ACCOUNT_UPDATE_MAX_SIZE: usize = 262_144;

#[test]
fn pool_library_assembles_and_fits_deployment_bound() {
    let lib = pool_library();
    let exports: Vec<String> = lib.exports().map(|e| e.path().to_string()).collect();
    for proc_name in ["swap", "mint", "burn", "collect"] {
        assert!(
            exports.iter().any(|e| e.ends_with(&format!("::{proc_name}"))),
            "pool library must export {proc_name}; exports: {exports:?}"
        );
    }
    let size = pool_library_size();
    println!("MASM pool component library serialized size: {size} bytes");
    assert!(
        size < ACCOUNT_UPDATE_MAX_SIZE,
        "pool component ({size} bytes) must stay below ACCOUNT_UPDATE_MAX_SIZE \
         ({ACCOUNT_UPDATE_MAX_SIZE}) for tx-deployability"
    );
}

#[test]
fn note_scripts_assemble_with_distinct_roots() {
    let roots: Vec<_> = [
        PoolNoteKind::Swap,
        PoolNoteKind::Mint,
        PoolNoteKind::Burn,
        PoolNoteKind::Collect,
    ]
    .into_iter()
    .map(|kind| note_script(kind).root())
    .collect();
    for i in 0..roots.len() {
        for j in (i + 1)..roots.len() {
            assert_ne!(roots[i], roots[j], "note script roots must be distinct");
        }
    }
}

#[test]
fn component_builds_with_thirteen_slots() {
    let init = PoolInitStorage {
        pool_config: Word::from([1u32, 2, 3, 4]),
        pool_params: Word::from([3000u32, 60, 0, 0]),
        p2id_root: Word::from([5u32, 6, 7, 8]),
        sqrt_price: Word::from([0u32, 0, 0, 1]),
        pool_state: Word::from([524288u32, 1, 0, 0]),
    };
    let component = component(&init);
    assert_eq!(component.storage_slots().len(), 13);
}

/// The pool's Poseidon2 5-element preimages (position keys, P2ID serials) must hash
/// identically in-VM (`miden::core::crypto::hashes::poseidon2::hash_elements`) and
/// host-side (`Hasher::hash_elements`) — the testbed's expected values depend on it.
#[test]
fn in_vm_hash_elements_matches_host_poseidon2() {
    let lib = amm_math_masm::assemble_library();
    let source = r#"
use miden::core::crypto::hashes::poseidon2
use miden::core::sys

begin
    repeat.5 adv_push end
    mem_store.4
    mem_store.3
    mem_store.2
    mem_store.1
    mem_store.0
    push.5 push.0
    exec.poseidon2::hash_elements
    exec.sys::truncate_stack
end
"#;
    let program = amm_math_masm::assemble_program(&lib, source);
    let preimages: [[u64; 5]; 3] = [
        [1, 2, 3, 4, 5],
        [0xDEADBEEF, 0xFFFF_FFFF, 524_288, 967_924, 0x504F_5331],
        [981273, 0, 1, u32::MAX as u64, 42],
    ];
    for preimage in preimages {
        // adv_push pops FIFO; the driver stores values to memory addresses 0..4 in
        // pop order, so advice[i] lands at address i.
        let stack = amm_math_masm::execute(&lib, &program, &preimage)
            .expect("hash driver must execute");
        let expected: Word = Hasher::hash_elements(
            &preimage.map(|value| Felt::new_unchecked(value)),
        );
        // stack top-first is h0..h3 (host Word order).
        let got: Vec<u64> = stack[0..4].to_vec();
        let want: Vec<u64> = (0..4).map(|i| expected[i].as_canonical_u64()).collect();
        assert_eq!(got, want, "hash mismatch for preimage {preimage:?}");
    }
}
