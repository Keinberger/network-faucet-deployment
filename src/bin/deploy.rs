use rand::RngCore;
use std::{fs, path::Path, sync::Arc};

use miden_client::{
    account::{
        component::{BasicWallet, NetworkFungibleFaucet},
        AccountBuilder, AccountStorageMode, AccountType,
    },
    asset::TokenSymbol,
    auth::{AuthRpoFalcon512, AuthSecretKey},
    builder::ClientBuilder,
    crypto::rpo_falcon512::SecretKey,
    keystore::FilesystemKeyStore,
    rpc::{Endpoint, GrpcClient},
    testing::Auth,
    transaction::TransactionRequestBuilder,
    ClientError, Felt,
};
use miden_client_sqlite_store::ClientBuilderSqliteExt;

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
    let alice_key_pair = SecretKey::with_rng(client.rng());

    // Build the account
    let builder = AccountBuilder::new(init_seed)
        .account_type(AccountType::RegularAccountUpdatableCode)
        .storage_mode(AccountStorageMode::Public)
        .with_auth_component(AuthRpoFalcon512::new(
            alice_key_pair.public_key().to_commitment().into(),
        ))
        .with_component(BasicWallet);

    let alice_account = builder.build().unwrap();

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
    // STEP 3: Create the network faucet account
    //------------------------------------------------------------

    let mut faucet_init_seed = [0_u8; 32];
    client.rng().fill_bytes(&mut faucet_init_seed);

    let network_faucet_component = NetworkFungibleFaucet::new(
        TokenSymbol::new("MDE").unwrap(),
        8,
        Felt::new(1_000_000),
        alice_account.id(),
    )
    .unwrap();

    // Build the account
    let builder = AccountBuilder::new(faucet_init_seed)
        .account_type(AccountType::FungibleFaucet)
        .storage_mode(AccountStorageMode::Network)
        .with_auth_component(Auth::IncrNonce)
        .with_component(network_faucet_component);

    let faucet_account = builder.build().unwrap();

    // Add the faucet to the client
    client.add_account(&faucet_account, false).await?;

    println!(
        "Faucet account created and added to client, ID: {:?}",
        faucet_account.id()
    );

    //------------------------------------------------------------
    // STEP 4: Deploy the network faucet contract using the increment nonce script
    //------------------------------------------------------------

    // Load the MASM script referencing the increment procedure
    let script_path = Path::new("./masm/deploy.masm");
    let script_code = fs::read_to_string(script_path).unwrap();

    let tx_script = client
        .script_builder()
        .compile_tx_script(&script_code)
        .unwrap();

    // Build a transaction request with the custom script
    let tx_deployment_request = TransactionRequestBuilder::new()
        .custom_script(tx_script)
        .build()
        .unwrap();

    // Execute and submit the transaction
    let tx_id = client
        .submit_new_transaction(faucet_account.id(), tx_deployment_request)
        .await
        .unwrap();

    println!(
        "View transaction on MidenScan: https://testnet.midenscan.com/tx/{:?}",
        tx_id
    );

    Ok(())
}
