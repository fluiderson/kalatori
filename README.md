## A gateway daemon for Kalatori

### Download

Compiled binaries for Linux x86-64 can be found in the "Releases" section.

### Compile from the source

To compile the daemon, the latest stable Rust compiler version is required. Then run the following command:

```sh
cargo b -r --workspace
```
Compiled binaries can be found in the `target/release` path.

### Structure & settings

The daemon for Kalatori consists of 2 variants:
- `kalatori` may be used for DOT, the native currency of the Polkadot and Polkadot Asset Hub chains.
- `kalatori-ah` may be used for the Polkadot Asset Hub chain and 2 of its assets: USDt (1984) & USD Coin (1337).

Both variants have almost the same startup environment variables:
- KALATORI_HOST: an address where the daemon opens its TCP socket server.
- KALATORI_SEED: a seed that's used as a base for the account derivation.
- KALATORI_DATABASE: a path to the daemon future/existing database.
> Note that a separate database file must be used for each supported currency, otherwise the database will be corrupted.
- KALATORI_RPC: an address of a Substrate RPC server.
- KALATORI_OVERRIDE_RPC: add this variable with any value to allow changing an RPC server address in the database.
- KALATORI_DECIMALS: set decimals for the chain native currency.
> Presents only in `kalatori`.
- KALATORI_USD_ASSET: sets which USD asset should be used. Possible value is "USDT" or "USDC".
> Presents only in `kalatori-ah`.
- KALATORI_DESTINATION: a hexadecimal address of the account that the daemon will send all payments to.

### Examples

A tipical command to run `kalatori` for the Polkadot chain may look like this:

```sh
KALATORI_HOST="127.0.0.1:16726" \
KALATORI_SEED="bottom drive obey lake curtain smoke basket hold race lonely fit walk" \
KALATORI_DATABASE="database.redb" \
KALATORI_RPC="wss://rpc.polkadot.io" \
KALATORI_DECIMALS="12" \
KALATORI_DESTINATION="0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" \
kalatori
```

And a command to run `kalatori-ah`for the Polkadot AssetHub chain may look like this:

```sh
KALATORI_HOST="127.0.0.1:16726" \
KALATORI_SEED="bottom drive obey lake curtain smoke basket hold race lonely fit walk" \
KALATORI_DATABASE="database-ah-usdc.redb" \
KALATORI_RPC="wss://polkadot-asset-hub-rpc.polkadot.io" \
KALATORI_DESTINATION="0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" \
KALATORI_USD_ASSET="USDC"
kalatori-ah
```

### Testing

[Chopsticks](https://github.com/AcalaNetwork/chopsticks) can be used to test the daemon out on a copy of a real network. This repository contains 2 config examples for testing:

### - Polkadot

Use the following command inside this repository root directory to run Chopstick with the Polkadot config example:

```sh
npx @acala-network/chopsticks@latest -c chopsticks/pd.yml
```

Then run `kalatori` with `KALATORI_RPC` set on the Chopsticks default server:

```sh
KALATORI_HOST="127.0.0.1:16726" \
KALATORI_SEED="bottom drive obey lake curtain smoke basket hold race lonely fit walk" \
KALATORI_RPC="ws://localhost:8000" \
KALATORI_DECIMALS="12" \
KALATORI_DESTINATION="0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" \
kalatori
```

### - Polkadot Asset Hub

Use the following command inside this repository root directory to run Chopstick with the Polkadot Asset Hub config example:

```sh
npx @acala-network/chopsticks@latest -c chopsticks/pd-ah.yml
```

Then run `kalatori-ah` with `KALATORI_RPC` set on the Chopsticks default server, and `KALATORI_USD_ASSET` set on the USD asset being tested:

```sh
KALATORI_HOST="127.0.0.1:16726" \
KALATORI_SEED="bottom drive obey lake curtain smoke basket hold race lonely fit walk" \
KALATORI_RPC="ws://localhost:8000" \
KALATORI_DESTINATION="0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" \
KALATORI_USD_ASSET="USDC"
kalatori-ah
```
