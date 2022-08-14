# melwalletd: Themelio wallet daemon

[![](https://img.shields.io/crates/v/melwalletd)](https://crates.io/crates/melwalletd)
![](https://img.shields.io/crates/l/melwalletd)

As with other UTXO-based blockchains like Bitcoin that lacks an "account" abstraction, Themelio requires somewhat involved logic for _wallets_ --- software that manages on-chain assets and provides an interface roughly similar to a bank account.

In Themelio, the "canonical" wallet software is **melwalletd**, a headless program that internally manages wallets and exposes a local REST API for operations on the wallets. Although you can use it directly as a Themelio wallet, melwalletd is intended more as a backend to wallet apps, such as[**melwallet-cli**](https://github.com/themeliolabs/melwallet-client), the official CLI wallet.

---

## Starting melwalletd

After installing melwalletd through `cargo install --locked melwalletd`, start it by giving it a directory in which wallets are stored and a network to which you want to connect:

```shell
$ melwalletd --wallet-dir ~/.themelio-wallets --network testnet
May 18 16:20:43.583  INFO melwalletd: opened wallet directory: []
```

If the directory doesn't exist it will be created. By default, melwalletd will start listening on `localhost:11773`.

---

## Managing wallets

### Creating a wallet

**Endpoint**
`PUT /wallets/[wallet name]`

**Body fields**

- `password`: optional field; password with which to encrypt the private key. **Warning**: if not given, private key will be stored in cleartext!
- `secret`: optional field; secret key used to import an existing wallet

**Response**

- Nothing

**Example**

```shell
$ curl -s 'localhost:11773/wallets/alice' -X PUT --data '{"password": "password"}'
```

### Listing all wallets

**Endpoint**
`GET /wallets`

**Response**

- Hashtable mapping wallet names to a type **WalletSummary** with fields:
  - `total_micromel`: total µMEL balance of wallet
  - `detailed_balance`: a map connecting specific coin denomiations to amounts
  - `staked_microsym`: the amount of µSYM staked on the network
  - `network`: 1 for testnet, 255 for mainnet
  - `address`: address-encoded covenant hash
  - `locked`: boolean saying whether or not the wallet is locked.

**Example**

```shell
$ curl -s localhost:11773/wallets | jq
{
  "alice": {
    "total_micromel": 0,
    "network": 1,
    "address": "t607gqktd3njqewnjcvzxv2m4ta6epbcv1sdjkp0qkmztaq3wxn350",
    "detailed_balance": {
		"73": 9871,
        "6d": 32131,
        "64": 314159265
      },
    "staked_microsym": 2500000000,

    "locked": true
  },
  "labooyah": {
    "total_micromel": 0,
    "network": 255,
    "address": "t1jhtj4ex1n069xr8w6mbkgrt25jgzw0pam1a25redg9ykpsykbq70",
    "detailed_balance": {
		"6d": 202220,
    },
    "staked_microsym": 0,
    "locked": true
  },
  "testnet": {
    "total_micromel": 17322999920,
    "network": 1,
    "address": "t6zf5m662ge2hwax4hcs5kzqmr1a5214fa9sj2rbtassw04n6jffr0",
    "detailed_balance": {
		"64": 10111,
    },
    "staked_microsym": 0,
    "locked": true
  }
}
```

### Dumping a wallet

**Endpoint**
`GET /wallets/[wallet name]`

**Response**

- Hashtable mapping wallet names to type **WalletSummary** with fields:
  - `total_micromel`: total µMEL balance of wallet
  - `detailed_balance`: a map connecting specific coin denomiations to amounts
  - `staked_microsym`: the amount of µSYM staked on the network
  - `network`: 1 for testnet, 255 for mainnet
  - `address`: address-encoded covenant hash
  - `locked`: boolean saying whether or not the wallet is locked.

**Example**

```shell
$ curl -s localhost:11773/wallets/alice | jq
{
  "alice": {
    "total_micromel": 0,
    "network": 1,
    "address": "t607gqktd3njqewnjcvzxv2m4ta6epbcv1sdjkp0qkmztaq3wxn350",
    "detailed_balance": {
		"73": 9871,
        "6d": 32131,
        "64": 314159265
      },
    "staked_microsym": 2500000000,

    "locked": true
  }
}
```

---

## Using a single wallet

### Unlocking a wallet

**Endpoint**
`POST /wallets/[name]/unlock`

**Body fields**

- `password`: password

**Response**

None

### Locking a wallet

**Endpoint**
`POST /wallets/[name]/lock`

**Body fields**

None

**Response**

None

### Sending a faucet transaction

This verb sends a faucet transaction that adds a fixed sum of 1001 MEL to the wallet.
**Note**: obviously, this only works with _testnet_ wallets!

**Endpoint**
`POST /wallets/[name]/send-faucet`

**Body fields**

None.

**Response**

Quoted hexadecimal transaction hash of the transaction being sent.

**Example**

```shell
$ curl -s localhost:11773/wallets/alice/send-faucet -X POST
"86588da7863b39152105e4f78c04e07a5d3f3ebf61d799f95293372dabdb06a1"
```

### Listing all transactions

**Endpoint**
`GET /wallets/[name]/transactions/`

**Response**

A JSON object containing a list with elements of type **(TxHash, Option\<BlockHeight\>)**:

- TxHash: hash of a particular transaction associated with a given wallet
- Option\<BlockHeight\>: **null** if unconfirmed, otherwise the integer height of the block where the transaction was confirmed

**Example**

```shell
$ curl -s localhost:11773/wallets/bar/transactions | jq
[
  [
    "d23b4240f7e02a38e8deb8a111d0ec8650a8912df50d15ec43992e3085b4ca98",
    null
  ],


    [
      "5b4ca98d23b4240f7e02a38e8deb8a111d0ec8650a8912df50d15ec43992e308",
      48113
    ]

]

```

### Checking on a transaction

**Endpoint**
`GET /wallets/[name]/transactions/[txhash]`

**Response**

A JSON of a type **TransactionStatus** with fields:

- `raw`: the actual transaction in JSON format
- `confirmed_height`: **null** if not confirmed, otherwise the height at which the transaction was confirmed.
- `outputs`: an array of **AnnCoinID** objects like:
  - `coin_data`: a CoinData object
  - `is_change`: is this a change output that goes to myself?
  - `coin_id`: a string-represented CoinID (txhash-index)

**Example**

```shell
$ curl -s localhost:11773/wallets/alice/transactions/
442bdc353b773b8949c59cee8061545a9a89e27a2e6638fcc1d065583bb170b8  | jq
{
  "raw": {
    "kind": 255,
    "inputs": [],
    "outputs": [
      {
        "covhash": "t4xn73csvjxp0dvh0gs6ehpgfq0ht0akr9g6tkxywv7xvfp09cy6x0",
        "value": 1001000000,
        "denom": "MEL",
        "additional_data": ""
      }
    ],
    "fee": 1001000000,
    "covenants": [],
    "data": "72a1aba97ada3d1958932244836b0c72aaedeef7f6b032c1877c83ae05e28469",
    "sigs": []
  },
  "confirmed_height": 42633,
  "outputs": [
    {
      "coin_data": {
        "covhash": "t4xn73csvjxp0dvh0gs6ehpgfq0ht0akr9g6tkxywv7xvfp09cy6x0",
        "value": 1001000000,
        "denom": "MEL",
        "additional_data": ""
      },
      "is_change": true,
      "coin_id": "442bdc353b773b8949c59cee8061545a9a89e27a2e6638fcc1d065583bb170b8-0"
    }
  ]
}
```

### Preparing a transaction

**Note**: This _prepares_ a transaction to be sent from a wallet, creating a filled-in, valid transaction, but _without_ changing the wallet state.
If the you actually want to send the transaction, the [send-tx](#sending-a-transaction) call must be used.

Keep in mind, this endpoint is not useful for more advanced covenant deployment. Check [send-tx](#sending-a-transaction)

**Endpoint**
`POST /wallets/[name]/prepare-tx`

**Body fields**

- `outputs`: an array of **CoinData**s, representing the desired outputs of the transaction. Any change outputs that are added are guaranteed to be added after these outputs.

- `kind`: _optional_ **TxKind** of the transaction (defaults to Normal)
- `inputs`: _optional_ an array of **CoinID**s, representing inputs that must be spent by this transaction. This is useful for building covenant chains and such.

- `data`: _optional_ additional data of the transaction (defaults to empty)
- `covenants`:
- `nobalance`: _optional_ vector of **Denom**s on which balancing --- checking that exactly the same number of coins are produce by a transaction as those consumed by it --- should not be done. Normally, this is used to exempt ERG balance from being checked when preparing a DOSC-minting transaction.
- `signing_key`: _optional_ an ed25519 signing key that corresponds to the wallet's covenant. **WARNING**: Only for advanced usecases, this can cause loss of funds if not used properly

**Response**

- JSON-encoded **Transaction**

**Example**

```shell
$ curl -s localhost:11773/wallets/alice/prepare-tx -X POST --data '{
    "outputs": [
        {
            "covhash": "57df1dd5b067f77a177127cbe4d69aa10fe8fcac3b2f9718cb8263d5a6216ab0",
            "value": 1000,
            "denom": "6d",
            "additional_data": ""
        }
    ],
    "signing_key": "4239e79eab9b39c49de990363197a64e1a54f0f9a0d12a936e85e69ea7fb05b006425dfe7967003e2a5362e36231730f2faaa6068979afc52784f916466e05b6"
}'


{
    "kind": 0,
    "inputs": [
        {
            "txhash": "4950f3af9da569e1a99a7e738026a581d6e96caaf02d94b02efcd645540a2d2f",
            "index": 0
        }
    ],
    "outputs": [
        {
            "covhash": "57df1dd5b067f77a177127cbe4d69aa10fe8fcac3b2f9718cb8263d5a6216ab0",
            "value": 1000,
            "denom": "6d",
            "additional_data": ""
        },
        {
            "covhash": "41d04b65a010aaa3404e1a109c53e3190a393d7ff920fa5644969a879547d0aa",
            "value": 1000998977,
            "denom": "6d",
            "additional_data": ""
        }
    ],
    "fee": 23,
    "scripts": [
        "420009f100000000000000000000000000000000000000000000000000000000000000064200005050f02006425dfe7967003e2a5362e36231730f2faaa6068979afc52784f916466e05b6420001320020"
    ],
    "data": "",
    "sigs": [
        "df3cc2795d3c9576b85c4c960501af3e44bbc490c9dc51c8422f18a3a0fa1150c2f745440206748c8205971ba0cff32b87c9797a4d1098ce3bd677db62e8560c"
    ]
}
```

### Sending a transaction

**Note**: This endpoint will reject any transactions that are malformed or don't belong to the wallet. For personal use recommend using [melwallet-cli]({{< ref my-first-tx.md>}}) instead. If you must use this endpoint, consider using [/prepare-tx](#preparing-a-transaction) to prepare the body of this transaction

**Endpoint**
`POST /wallets/[name]/send-tx`

**Body**

JSON-encoded **Transaction** with fields:

- kind: **TXKind** of this transaction
- inputs: an array of **CoinID**s
- outputs: an array of **CoinData**s
- `fee`: **CoinValue**

**Response**

- Quoted transaction hash
