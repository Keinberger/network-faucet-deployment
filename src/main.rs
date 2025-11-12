use rand::RngCore;
use std::sync::Arc;

use miden_client::{
    account::component::{BasicWallet, NetworkFungibleFaucet},
    account::AccountId,
    account::{AccountBuilder, AccountStorageMode, AccountType},
    asset::{Asset, FungibleAsset, TokenSymbol},
    auth::{AuthRpoFalcon512, AuthSecretKey},
    builder::ClientBuilder,
    crypto::rpo_falcon512::SecretKey,
    keystore::FilesystemKeyStore,
    note::{
        Note, NoteAssets, NoteError, NoteExecutionHint, NoteInputs, NoteMetadata, NoteRecipient,
        NoteTag, NoteType, WellKnownNote,
    },
    rpc::{Endpoint, GrpcClient},
    transaction::{OutputNote, TransactionRequestBuilder},
    ClientError, Felt, Word,
};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_lib::note::create_mint_note;

fn create_p2id_note_exact(
    sender: AccountId,
    target: AccountId,
    assets: Vec<Asset>,
    note_type: NoteType,
    aux: Felt,
    serial_num: Word,
) -> Result<Note, NoteError> {
    let note_script = WellKnownNote::P2ID.script();
    let note_inputs = NoteInputs::new(vec![target.suffix(), target.prefix().as_felt()])?;
    let recipient = NoteRecipient::new(serial_num, note_script, note_inputs);

    let tag = NoteTag::from_account_id(target);

    let metadata = NoteMetadata::new(sender, note_type, tag, NoteExecutionHint::always(), aux)?;
    let vault = NoteAssets::new(assets)?;

    Ok(Note::new(vault, metadata, recipient))
}

#[tokio::main]
async fn main() -> Result<(), ClientError> {
    // Initialize client & keystore
    // let endpoint = Endpoint::new("http".into(), "localhost".into(), Some(57291));
    let endpoint = Endpoint::testnet();
    let timeout_ms = 10_000;
    let rpc_client = Arc::new(GrpcClient::new(&endpoint, timeout_ms));
    let keystore: FilesystemKeyStore<rand::prelude::StdRng> =
        FilesystemKeyStore::new("./keystore".into()).unwrap().into();

    let mut client = ClientBuilder::new()
        .rpc(rpc_client)
        .sqlite_store("./store.sqlite3".into())
        // .store(StoreBuilder::Factory(Box::new(SqliteStoreFactory::new(
        //     "./store.sqlite3",
        // ))))
        .authenticator(keystore.clone().into())
        .in_debug_mode(true.into())
        .build()
        .await?;

    let sync_summary = client.sync_state().await.unwrap();
    println!("Latest block: {}", sync_summary.block_num);

    //------------------------------------------------------------
    // STEP 1: Create a basic wallet for Alice
    //------------------------------------------------------------
    println!("\n[STEP 1] Creating a new account for Alice");

    // Account seed
    let mut init_seed = [0_u8; 32];
    client.rng().fill_bytes(&mut init_seed);

    let key_pair = SecretKey::with_rng(client.rng());

    // Build the account
    let builder = AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthRpoFalcon512::new(
            key_pair.public_key().to_commitment().into(),
        ))
        .with_component(BasicWallet);

    let alice_account = builder.build().unwrap();

    // Add the account to the client
    client.add_account(&alice_account, false).await?;

    // Add the key pair to the keystore
    keystore
        .add_key(&AuthSecretKey::RpoFalcon512(key_pair))
        .unwrap();

    println!(
        "Alice account created and added to client, ID: {:?}",
        alice_account.id()
    );

    //------------------------------------------------------------
    // STEP 3: Create the network faucet account
    //------------------------------------------------------------

    let mut init_seed = [0_u8; 32];
    client.rng().fill_bytes(&mut init_seed);
    let faucet_key_pair = SecretKey::with_rng(client.rng());

    // Build the account
    let builder = AccountBuilder::new(init_seed)
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthRpoFalcon512::new(
            faucet_key_pair.public_key().to_commitment().into(),
        ))
        .with_component(
            NetworkFungibleFaucet::new(
                TokenSymbol::new("MID").unwrap(),
                8,
                Felt::new(1_000_000),
                alice_account.id(),
            )
            .unwrap(),
        );

    let faucet_account = builder.build().unwrap();

    // Add the faucet to the client
    client.add_account(&faucet_account, false).await?;

    // Add the key pair to the keystore
    keystore
        .add_key(&AuthSecretKey::RpoFalcon512(faucet_key_pair))
        .unwrap();

    println!(
        "Faucet account created and added to client, ID: {:?}",
        faucet_account.id()
    );

    //------------------------------------------------------------
    // STEP 4: Issue MINT note from network faucet to alice
    //------------------------------------------------------------

    let stored_owner_word = faucet_account.storage().get_item(2).unwrap();
    let stored_owner_id = AccountId::new_unchecked([stored_owner_word[3], stored_owner_word[2]]);

    // Compute the output P2ID note
    let amount = 50;
    let mint_asset = FungibleAsset::new(faucet_account.id(), amount)
        .unwrap()
        .into();
    let aux = Felt::new(27);
    let serial_num = Word::default();

    let output_note_tag = NoteTag::from_account_id(alice_account.id());
    let p2id_mint_output_note = create_p2id_note_exact(
        faucet_account.id(),
        alice_account.id(),
        vec![mint_asset],
        NoteType::Public,
        aux,
        serial_num,
    )?;

    let recipient = p2id_mint_output_note.recipient().digest();

    let mint_note = create_mint_note(
        faucet_account.id(),
        stored_owner_id.into(),
        recipient,
        output_note_tag.into(),
        Felt::new(amount),
        aux,
        aux,
        client.rng(),
    )
    .unwrap();

    let mint_transaction_request = TransactionRequestBuilder::new()
        .own_output_notes(vec![OutputNote::Full(mint_note)])
        .build()
        .unwrap();

    let mint_transaction_result = client
        .submit_new_transaction(faucet_account.id(), mint_transaction_request)
        .await
        .unwrap();

    println!("MINT TX successfully submitted");

    println!("Waiting for 15 seconds...");

    tokio::time::sleep(std::time::Duration::from_secs(15)).await;

    // Craft transaction to consume the newly created P2ID note
    let consume_p2id_note_transaction_request = TransactionRequestBuilder::new()
        .unauthenticated_input_notes(vec![(p2id_mint_output_note, None)])
        .build()
        .unwrap();

    let consume_transaction_result = client
        .submit_new_transaction(alice_account.id(), consume_p2id_note_transaction_request)
        .await
        .unwrap();

    println!("CONSUME TX successfully submitted");

    Ok(())
}
