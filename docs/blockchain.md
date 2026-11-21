# Blockchain Payments and Wallet Identity (Parked Design)

**Status:** not implemented. Nothing in the code base uses a blockchain,
wallet or payment network today. This document collects every
blockchain-related design idea from the original design spec so a future
session can pick it up in one place.

What *is* implemented is the in-game credit system that blockchain
deposits would feed. See [Attaching to the current credit system](#attaching-to-the-current-credit-system).

Related research: [research/mobile-iap-blockchain-bridge.md](research/mobile-iap-blockchain-bridge.md)
(bridging iOS/Android in-app purchases to on-chain settlement).

---

## 1. Original goals

The original spec (when the project was called HemiMUD) listed these
blockchain-related principles:

- **Blockchain payments** via the "Hemi Payment Hub", a pull-payment
  contract system on the Hemi network. Its separate spec
  (`hemi-payment-hub-spec.md`) was referenced but never written or
  committed.
- **Pull payments:** users deposit collateral into the hub and grant
  vendors a budget; vendors pull funds (inverting the usual push model).
- **Wallet = identity:** an Ethereum wallet is the permanent,
  cross-universe account identity.
- **Universe economic isolation:** credits are universe-scoped, and
  crypto → credits conversion is one-way.

Identity model as originally designed:

- Global layer: account (wallet), friends, platform subscription.
- Per universe: separate player object, credits, stats, inventory.

## 2. Payment Hub integration points

- **Platform vendor wallet:** receives universe creation fees and
  hosting fees. Configured per node, e.g. `owner_wallet = "0x..."`.
- **Universe vendor wallet:** each universe owner registers as a vendor
  and receives that universe's credit purchases.
- **Credit purchase:** client calls `hub.pull(user, token, usdAmount)`
  via the universe's vendor wallet.
- Node configuration would include an RPC endpoint, e.g.
  `hemi_rpc_url = "https://rpc.hemi.network"`.

### Why credits are one-way (crypto → credits, no withdrawal)

- Avoids money-transmitter regulation.
- Simplifies accounting.
- Prevents arbitrage between universes.
- Credits are consumable game resources, not stored value.

### Purchase flow

1. User clicks "Buy 1000 credits for $10".
2. Client calls Payment Hub `pull(user, USDC, 10_000000)`.
3. On success the server credits the user's universe account via a
   replicated (Raft) write.
4. On failure the UI shows a "top up collateral" message.

### Credit packages (universe-configurable)

```json
[
  { "usd": 5, "credits": 500 },
  { "usd": 20, "credits": 2200, "bonus_pct": 10 },
  { "usd": 100, "credits": 12000, "bonus_pct": 20 }
]
```

Default action costs were drafted in US cents (Venice fast tier 2¢,
smart tier 20¢, vision 10¢, image 5¢, room image 7¢, object creation
10¢, bot monthly fee 50¢, class definition $50). None of these are
charged automatically today.

## 3. Wallet authentication

Planned to replace username/password login:

1. Client requests a challenge: `POST /api/auth/challenge -> { challenge, expires }`.
2. Server returns a nonce message; client signs it with the wallet.
3. Client submits it: `POST /api/auth/verify -> { token, account }`.
   Server verifies the signature (ethers-rs or alloy) and issues a
   session token (JWT was proposed).
4. Account is created on first successful login.

Planned tests: valid wallet auth flow, signature from a different wallet
rejected, account created on first login.

Today the server uses username/password accounts with opaque random
session tokens (`mudd/src/auth/`).

## 4. Data model additions

Columns and records the original spec proposed:

| Where | Addition |
|-------|----------|
| `accounts` | `wallet_address TEXT UNIQUE NOT NULL`, `platform_tier TEXT DEFAULT 'free'` |
| `universes` | `vendor_wallet TEXT NOT NULL` |
| player object | `wallet_address` property |
| credits | `lifetime_deposited` |
| `credit_transactions` | a `deposit` ledger row type (the general spend/refund/grant ledger is a non-blockchain roadmap item) |

Typed Raft log entries proposed:

```rust
CreateUniverse { universe_id, name, owner_account_id, vendor_wallet, initial_config },
DepositCredits { universe_id, account_id, wallet_address, amount, tx_hash, block_number },
```

(The implemented Raft log carries generic SQL write requests, not typed
entries; see `mudd/src/raft/types.rs`.)

Endpoint proposed: `POST /api/credits/purchase -> { success, new_balance }`.

## 5. Client

- Wallet connection with wagmi + viem (`WalletConnect.tsx`,
  `usePaymentHub.ts`, `lib/wallet.ts`).
- Payment Hub deposit/budget UI and a credit balance display.
- Planned Playwright specs: wallet connection/login and a credit
  purchase flow using a wallet mock (`connectWallet`,
  `approveTransaction`).

## 6. Contracts and tooling

A `contracts/` Foundry project was planned:

```
contracts/
├── foundry.toml
├── src/{PaymentHub.sol, PaymentHubLogic.sol, PriceOracle.sol}
├── script/Deploy.s.sol
└── test/{PaymentHub.t.sol, PriceOracle.t.sol}
```

Plus `scripts/deploy-contracts.sh` (testnet/mainnet), `forge build`,
`forge test` and `forge fmt --check` in the build, and a CI job
running `forge test`. Server-side: `payments/hub_client.rs`, a mock
hub for tests, and pre-funded test wallet fixtures.

## Attaching to the current credit system

What exists (`mudd/src/credits/mod.rs`):

- Table `credits(id, universe_id, player_id, balance, UNIQUE(universe_id, player_id))`.
- `CreditManager::get_balance`, `deduct(universe, account, amount, reason)`,
  `grant(universe, account, amount, reason)`, `set_balance`. Writes are
  replicated through the Raft writer as SQL.
- Lua: `game.get_credits()`, `game.deduct_credits(amount, reason)`,
  `game.admin_grant_credits(account_id, amount)` (admin only).

How a deposit would attach:

1. A deposit watcher (or purchase endpoint) confirms an on-chain payment
   to the universe's vendor wallet.
2. It maps the payer (wallet address or a memo naming the account) to an
   account id.
3. It calls `CreditManager::grant(universe, account, credits, "deposit:<tx_hash>")`.
4. Idempotency requires a ledger keyed by `tx_hash` (the
   `credit_transactions` table above), so a replayed or re-orged event
   cannot double-credit.

## Open questions

- Which chain: Hemi, Base or Ethereum mainnet? USDC availability and
  tooling differ.
- Keep username/password accounts and *link* a wallet, or make the
  wallet the identity?
- Pull-payment hub contract vs. plain ERC-20 transfers with a memo.
- Confirmation depth and re-org handling before granting credits.
- App-store rules for iOS/Android clients (see the research note).
- Regulatory review of the one-way credits model.
