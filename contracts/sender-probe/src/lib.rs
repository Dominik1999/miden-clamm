// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

// Phase 2 gate test (DESIGN.md Part 5, assumption 1): calling
// `miden::active_note::get_sender()` from INSIDE a `#[component]` impl
// (account code, during note consumption) must compile, link, and return
// the note creator's AccountId.

use miden::{active_note, component, component_storage, felt, Felt, StorageValue, Word};

/// Storage layout for the sender-probe component.
#[component_storage]
struct SenderProbeStorage {
    /// Last note sender recorded by `record_sender`, as the protocol Word
    /// layout of an AccountId: `[0, 0, suffix, prefix]`.
    #[storage(description = "last recorded note sender account id")]
    last_sender: StorageValue<Word>,
}

/// API of the sender-probe account component.
#[component]
trait SenderProbe {
    /// Reads the active note's sender from kernel state (NOT from any
    /// script-passed argument) and records it into account storage.
    /// Returns Felt(1) on success.
    fn record_sender(&mut self) -> Felt;
}

#[component]
impl SenderProbe for SenderProbeStorage {
    fn record_sender(&mut self) -> Felt {
        // The load-bearing call: kernel-read sender, from component context.
        let sender = active_note::get_sender();
        // `From<AccountId> for Word` produces `[0, 0, suffix, prefix]`.
        let sender_word: Word = sender.into();
        self.last_sender.set(sender_word);
        felt!(1)
    }
}
