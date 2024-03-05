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
