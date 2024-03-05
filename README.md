To compile the daemon, the latest stable Rust compiler version is required. Then run the following command:
```bash
cargo b -r --workspace
```
Compiled binaries may be found in the `target/release` path.

The daemon for Kalatori consists of 2 variants:
- `kalatori` may be used for the Polkadot chain and DOT, its native currency.
- `kalatori-ah` may be used for the Polkadot Asset Hub chain and the USDT (1984) asset.

Both variants have almost the same startup environment variables:
- KALATORI_HOST: an address where the daemon will open its TCP socket server.
- KALATORI_SEED: a seed that will be used as a base for the account derivation.
- KALATORI_DATABASE: a path to the daemon future/existing database.
- KALATORI_RPC: an address of a Substrate RPC server.
- KALATORI_OVERRIDE_RPC: add this variable to allow changing RPC server address in the database.
- KALATORI_DECIMALS: set decimals for the chain native currency (presents only in `kalatori`).
- KALATORI_DESTINATION: a hexadecimal address of the account that the daemon will send all payments to.

For example, a tipical command to run `kalatori` may look like this:
```bash
KALATORI_HOST="127.0.0.1:16726" \
KALATORI_SEED="bottom drive obey lake curtain smoke basket hold race lonely fit walk" \
KALATORI_DATABASE="database.redb" \
KALATORI_RPC="wss://rpc.polkadot.io" \
KALATORI_DECIMALS="12" \
KALATORI_DESTINATION="0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" \
kalatori
```

And a command to run `kalatori-ah` may look like this:

```bash
KALATORI_HOST="127.0.0.1:16726" \
KALATORI_SEED="bottom drive obey lake curtain smoke basket hold race lonely fit walk" \
KALATORI_DATABASE="database.redb" \
KALATORI_RPC="wss://polkadot-asset-hub-rpc.polkadot.io" \
KALATORI_DESTINATION="0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" \
kalatori-ah
```
