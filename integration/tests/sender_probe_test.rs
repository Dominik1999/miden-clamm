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

/// Phase 2 gate test (DESIGN.md Part 5, assumption 1): calling
/// `miden::active_note::get_sender()` from INSIDE a `#[component]` impl
/// (account code, during note consumption) compiles, links, and returns the
/// note creator's AccountId.
///
/// The probe note passes no sender information whatsoever; the component
/// reads the sender from kernel state and writes it to storage. The test
/// asserts the committed storage holds exactly the known sender's account ID.
#[tokio::test]
async fn sender_probe_test() -> anyhow::Result<()> {
    let mut builder = MockChain::builder();

    // Create note sender account -- the KNOWN sender whose ID must appear in storage.
    let sender = builder.add_existing_wallet(Auth::BasicAuth {
        auth_scheme: AuthSchemeId::Falcon512Poseidon2,
    })?;

    // Build contracts
    let contract_package = Arc::new(build_project_in_dir(
        Path::new("../contracts/sender-probe"),
        true,
    )?);
    let note_package = Arc::new(build_project_in_dir(
        Path::new("../contracts/probe-note"),
        true,
    )?);

    // Seed the `last_sender` value slot with a zero Word (uninitialized).
    let last_sender_slot = StorageSlotName::new("sender_probe::sender_probe::last_sender")
        .context("invalid sender-probe storage slot name")?;
    let mut init_storage_data = InitStorageData::default();
    init_storage_data.insert_value(
        StorageValueName::from_slot_name(&last_sender_slot),
        Word::default(),
    )?;

    let probe_component = AccountComponent::from_package(&contract_package, &init_storage_data)
        .context("failed to build account component from sender-probe package")?;
    let probe_account = builder.add_account_from_builder(
        Auth::BasicAuth {
            auth_scheme: AuthSchemeId::Falcon512Poseidon2,
        },
        AccountBuilder::new([5_u8; 32])
            .account_type(AccountType::Public)
            .with_component(probe_component),
        AccountState::Exists,
    )?;

    let mut note_rng = RandomCoin::new(Word::from(
        NoteScript::from_package(note_package.as_ref())
            .context("failed to build note script from package")?
            .root(),
    ));
    let probe_note = NoteBuilder::new(sender.id(), &mut note_rng)
        .package((*note_package).clone())
        .build()
        .context("failed to build probe note from package")?;

    // Add the note to the mockchain and build it
    builder.add_output_note(RawOutputNote::Full(probe_note.clone()));
    let mut mock_chain = builder.build()?;

    // Consume the note with the probe account
    let tx_context = mock_chain
        .build_tx_context(probe_account.clone(), &[probe_note.id()], &[])?
        .build()?;
    let executed_transaction = tx_context.execute().await?;

    mock_chain.add_pending_executed_transaction(&executed_transaction)?;
    mock_chain.prove_next_block()?;

    // The guest-side `From<AccountId> for Word` layout is [0, 0, suffix, prefix].
    let recorded = mock_chain
        .committed_account(probe_account.id())?
        .storage()
        .get_item(&last_sender_slot)
        .expect("failed to read last_sender storage slot");

    let expected = Word::new([
        Felt::new(0)?,
        Felt::new(0)?,
        sender.id().suffix(),
        Felt::from(sender.id().prefix()),
    ]);

    assert_eq!(
        recorded, expected,
        "storage does not hold the sender's account id: recorded {recorded:?}, expected {expected:?} (sender {:?})",
        sender.id()
    );
    Ok(())
}
