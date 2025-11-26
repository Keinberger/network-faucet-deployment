use std::{sync::Arc, time::Duration};

use miden_client::{
    account::{component::BasicWallet, AccountBuilder, AccountId, AccountStorageMode, AccountType},
    asset::{Asset, FungibleAsset},
    auth::{AuthRpoFalcon512, AuthSecretKey, TransactionAuthenticator},
    builder::ClientBuilder,
    crypto::{rpo_falcon512::SecretKey, FeltRng},
    keystore::FilesystemKeyStore,
    note::{
        Note, NoteAssets, NoteError, NoteExecutionHint, NoteInputs, NoteMetadata, NoteRecipient,
        NoteTag, NoteType, WellKnownNote,
    },
    rpc::{Endpoint, GrpcClient},
    store::TransactionFilter,
    transaction::{OutputNote, TransactionId, TransactionRequestBuilder, TransactionStatus},
    Client, ClientError, Felt, Word,
};
use miden_client_sqlite_store::ClientBuilderSqliteExt;
use miden_lib::note::create_mint_note;
use rand::RngCore;

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

/// Waits for a transaction to be committed by the network.
async fn wait_for_transaction<AUTH: TransactionAuthenticator + Sync + 'static>(
    client: &mut Client<AUTH>,
    transaction_id: TransactionId,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    loop {
        client.sync_state().await?;

        let tracked_transaction = client
            .get_transactions(TransactionFilter::Ids(vec![transaction_id]))
            .await
            .map_err(|err| {
                format!(
                    "Failed to fetch transaction status while waiting for commitment: {}",
                    err
                )
            })?
            .pop()
            .ok_or_else(|| {
                format!(
                    "Transaction with ID {} not found while waiting for commitment",
                    transaction_id
                )
            })?;

        match tracked_transaction.status {
            TransactionStatus::Committed { block_number, .. } => {
                println!("Transaction committed at block {block_number}.");
                return Ok(());
            }
            TransactionStatus::Pending => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            TransactionStatus::Discarded(cause) => {
                return Err(format!(
                    "Transaction was discarded while waiting for commitment. Cause: {cause:?}"
                )
                .into());
            }
        }
    }
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
    let alice_key_pair = SecretKey::with_rng(client.rng());

    // Build the account
    let builder = AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthRpoFalcon512::new(
            alice_key_pair.public_key().to_commitment().into(),
        ))
        .with_component(BasicWallet);

    let mut alice_account = builder.build().unwrap();

    // Add the account to the client
    client.add_account(&alice_account, false).await?;

    // Add the key pair to the keystore
    keystore
        .add_key(&AuthSecretKey::RpoFalcon512(alice_key_pair))
        .unwrap();

    println!(
        "Alice account created and added to client, ID: {:?}",
        alice_account.id()
    );

    //------------------------------------------------------------
    // STEP 2: Define the network faucet account ID
    //------------------------------------------------------------
    let faucet_account_id = AccountId::from_hex("0xd8e3fa793ea82360734ec91a98e798").unwrap();
    let faucet_details = client.get_account(faucet_account_id.into()).await.unwrap();

    let faucet = if let Some(account_record) = faucet_details {
        // Clone the account to get an owned instance
        let account = account_record.account().clone();
        println!(
            "Account details: {:?}",
            account.storage().slots().first().unwrap()
        );
        account
    } else {
        panic!("Faucet not found!");
    };

    //------------------------------------------------------------
    // STEP 4: Issue MINT note from network faucet to alice
    //------------------------------------------------------------

    let stored_owner_word = faucet.storage().get_item(2).unwrap();
    let stored_owner_id = AccountId::new_unchecked([stored_owner_word[3], stored_owner_word[2]]);

    // Compute the output P2ID note
    let amount = 50;
    let mint_asset = FungibleAsset::new(faucet.id(), amount).unwrap().into();
    let aux = Felt::new(27);
    let serial_num = client.rng().draw_word();

    let output_note_tag = NoteTag::from_account_id(alice_account.id());
    let p2id_mint_output_note = create_p2id_note_exact(
        faucet_account_id,
        alice_account.id(),
        vec![mint_asset],
        NoteType::Private,
        aux,
        serial_num,
    )?;

    println!(
        "P2ID OUTPUT NOTE COMMITMENT: {:?}",
        p2id_mint_output_note.commitment().to_hex()
    );

    let recipient = p2id_mint_output_note.recipient().digest();

    let mint_note = create_mint_note(
        faucet.id(),
        stored_owner_id.into(),
        recipient,
        output_note_tag.into(),
        Felt::new(amount),
        aux,
        aux,
        client.rng(),
    )?;

    println!(
        "MINT NOTE COMMITMENT: {:?}",
        mint_note.commitment().to_hex()
    );

    let mint_transaction_request = TransactionRequestBuilder::new()
        .own_output_notes(vec![OutputNote::Full(mint_note)])
        .build()
        .unwrap();

    let mint_transaction_id = client
        .submit_new_transaction(stored_owner_id, mint_transaction_request)
        .await
        .unwrap();

    println!(
        "MINT TX successfully submitted: {:?}",
        mint_transaction_id.to_hex()
    );

    println!("Waiting for MINT transaction to be committed...");

    // tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    wait_for_transaction(&mut client, mint_transaction_id)
        .await
        .unwrap();

    // Craft transaction to consume the newly created P2ID note
    let consume_p2id_note_transaction_request = TransactionRequestBuilder::new()
        .unauthenticated_input_notes(vec![(p2id_mint_output_note, None)])
        .build()
        .unwrap();

    let consume_transaction_id = client
        .submit_new_transaction(alice_account.id(), consume_p2id_note_transaction_request)
        .await
        .unwrap();

    println!(
        "CONSUME TX successfully submitted: {:?}",
        consume_transaction_id.to_hex()
    );

    println!("Waiting for CONSUME transaction to be committed...");

    wait_for_transaction(&mut client, consume_transaction_id)
        .await
        .unwrap();

    client.sync_state().await.unwrap();

    alice_account = client
        .get_account(alice_account.id())
        .await
        .unwrap()
        .unwrap()
        .into();

    // print vault assets
    let asset_balance = alice_account
        .vault()
        .get_balance(faucet_account_id)
        .unwrap();
    println!("Vault assets: {:?}", asset_balance);

    Ok(())
}
