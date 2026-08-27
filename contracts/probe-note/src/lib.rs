// Do not link against libstd (i.e. anything defined in `std::`)
#![no_std]
#![feature(alloc_error_handler)]

use miden::*;

/// Native account of the note: exposes the `sender-probe` component methods
/// gathered from the `sender-probe` package.
#[account(sender_probe::SenderProbe)]
pub struct Wallet;

#[note]
struct ProbeNote;

#[note]
impl ProbeNote {
    #[note_script]
    fn run(self, _arg: Word, account: &mut Wallet) {
        // The note passes NOTHING about the sender: the component reads it
        // from kernel state via `active_note::get_sender()`.
        let result = account.record_sender();
        assert_eq(result, Felt::from_u32(1));
    }
}
