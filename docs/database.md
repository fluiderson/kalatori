## 1

**All slots are encoded with `parity-scale-codec = "3"`.**

### Tables

* `root`
```rs
type Key = String;
type Value = Vec<u8>;
```

Public keys of all active accounts.
* `keys`
```rs
/// A public key.
type Key = [u8; 32];
/// An accounts amount (never equals 0).
type Value = U256;
```

* `chains`
```rs
/// A chain hash.
type Key = [u8; 32];
/// A last scanned block.
type Value = Compact<u64>;
```

* `invoices`
```rs
/// An invoice name.
type Key = Vec<u8>;
type Value = Invoice;
```

* `accounts[chain_hash]`
```rs
/// An asset ID & an account ID.
type Key = (u128, [u8; 32]);
/// An invoice name.
type Value = Vec<u8>;
```

* `transactions[chain_hash]`
```rs
/// A death block.
type Key = Compact<u64>;
/// Pending transactions.
/// An account ID, a transaction nonce, a transaction.
type Values = ([u8; 32], u32, ???);
```

* `hit_list`
```rs
/// A creation timestamp.
type Key = u64;
/// A chain hash, an asset ID, and an account ID.
type Value = ([u8; 32], u128, [u8; 32]);
```

* `archive`
```rs
???
```

### `root` keyed untyped slots

* `db_version`
```rs
Compact<u64>
```

* `daemon_info`
```rs
struct DaemonInfo {
    /// A chain name, genesis hash, database hash, and type.
    chains: Vec<(String, String, [u8; 32], ChainType)>,
    /// The current public key.
    current_key: [u8; 32],
}
```

### Typed slots

```rs
struct Invoice {
    /// A public key & a derivation hash.
    derivation: ([u8; 32], [u8; 32]),
    status: InvoiceStatus,
    /// A creation timestamp.
    timestamp: u64
}

enum InvoiceStatus {
    Unpaid(u128),
    Paid,
}

??? ChainType {
    ???
}
```

## 0

**All slots are encoded with `parity-scale-codec = "3"`.**

### Tables

* `root`
```rs
type Key = String;
type Value = Vec<u8>;
```

* `invoices`
```rs
type Key = [u8; 32];
type Value = Invoice;
```

### `root` keyed untyped slots

* `db_version`
```rs
Compact<u64>
```

* `daemon_info`
```rs
struct DaemonInfo {
    rpc: String,
    key: [u8; 32],
}
```

* `last_block`
```rs
Compact<u32>
```

### Typed slots

```rs
struct Invoice {
    recipient: [u8; 32],
    order: [u8; 32],
    status: InvoiceStatus,
}

enum InvoiceStatus {
    Unpaid(u128),
    Paid(u128),
}
```
