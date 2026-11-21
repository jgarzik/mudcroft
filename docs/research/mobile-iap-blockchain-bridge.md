# Bridging Mobile In-App Purchases to On-Chain Universe Settlement

Date: 2026-11-21
Status: research note, not a decision. Not legal or tax advice.
Related: `docs/blockchain.md` (parked vendor-wallet design; this note does not depend on it).

## TL;DR

- Apple and Google will not move crypto for us, and they pay developers 2 to 9 weeks after the purchase. Any design that sends USDC to a universe owner the moment a player taps "Buy" needs us to put up that money in advance (prefunding). Any design that waits for the payout needs none.
- **The key reframing:** split *crediting the player* from *paying the universe owner*. Credits can be granted in seconds based on a server-verified, Apple- or Google-signed transaction. The dollars can settle later, in batches, on-chain. The on-chain "credit user foo N credits" message then serves as an **accounting record and receivable**. It is not a real-time payment.
- **Recommendation: Solution A, "Credit now, settle on payout day".** Mudcroft (the operator) is the seller of record in both stores. It credits players immediately and pays each universe owner a revenue share in USDC on Base. Payments go out in one batched transfer per payout cycle, along with an on-chain settlement manifest listing every credit grant. Float needed: zero (only a refund reserve, held back from owners' own money). Players never see a wallet. This is the Roblox DevEx / Telegram Stars model with a stablecoin rail, and it is the most proven of the options.
- Add **Solution B, a signed-receipt attestation ledger**, if universe owners want trust-minimized, real-time on-chain visibility. **Solution C, prefunded instant USDC**, is an optional paid tier that needs about 2 months of net revenue as capital.
- Settle on **Base** (native USDC, Circle CCTP, Coinbase/Bridge/Circle Mint rails). Hemi currently has only bridged USDC.e (as of 2026-11), so treat Hemi as a delivery destination reached by bridging, not as the primary settlement chain.

## 1. Background: app store payment facts

### Rules on digital goods and crypto

- **Apple 3.1.1:** in-game currencies must use In-App Purchase (IAP). Apps "may not use their own mechanisms to unlock content or functionality, such as ... cryptocurrencies and cryptocurrency wallets". Credits bought via IAP "may not expire". IAP may be used to "tip" digital content providers. The NFT rule: IAP may sell NFT-related services, and owned NFTs may not unlock functionality. Since the US exception, US-storefront apps may link out from NFT browsing. [Apple guidelines]
- **Apple 3.1.1(a) and 3.1.3:** US-storefront apps may include buttons, external links, or calls to action to other purchase methods without an entitlement. Elsewhere, link-outs need region-specific StoreKit External Purchase entitlements. App Review has said these changes did not alter *when* IAP is required. [Apple guidelines; Apple dev forums]
- **Apple 3.1.5 (cryptocurrencies):** wallets are allowed only for organization accounts. Exchange functionality is allowed only where licensed. ICO and crypto-securities apps must come from regulated institutions. No crypto rewards for tasks. Nothing in 3.1.5 lets an app sell game credits for crypto, or let crypto unlock content. [Apple guidelines, current as of 2026-11]
- **Google Play, blockchain-based content (since 2023):** an in-app product that *is* a tokenized digital asset must be declared in Play Console. No earnings promotion, no randomized NFT purchases without gambling eligibility. Crypto exchanges and wallets must comply with country-specific licensing (an updated Help Center list took effect 2026-01-28). [Google Play policy]
- **Implication for Mudcroft:** a credit pack bought through IAP is an ordinary consumable. As long as the *player-facing* item is not a token and is not withdrawable, neither store's crypto rules come into play. Everything blockchain-related happens between Mudcroft and universe owners, outside the app.

### Commissions (rapidly changing; as of 2026-11)

| Regime | Rate | Notes |
|---|---|---|
| Apple IAP, standard / Small Business Program | 30% / 15% | Small Business Program applies under $1M in prior-year proceeds |
| Apple US link-out | 0% today, possibly 15% later | Dec 2025: the Ninth Circuit upheld contempt but allowed a cost-based commission, sent back to the district court to set. Apr 2026: the stay was reversed, so the zero-commission injunction is active. Aug 2026: Apple proposed 15% on remand. Supreme Court petition No. 25-1311 status unverified. |
| Apple EU (new terms effective 2026-10-01) | IAP 26% (15% small business). Alternative payment provider 20% (10%). Link-out 15% (10%). 5% Core Technology Commission for distribution outside the App Store | Replaces the Core Technology Fee, initial acquisition fee, and store services fee |
| Apple Japan (Mobile Software Competition Act, 2025-12-17) | IAP 21% + 5% processing. Small business 10% (+5%?). Link-out 15% (10%). Alternative marketplace 5% Core Technology Commission | Apple authorizes alternative marketplaces |
| Google Play, new model (from 2026-06-30 in US/EEA/UK) | 20% service fee (15% new installs for participants), 10% on the first $1M, plus a 5% billing fee if using Play Billing | From the Epic v. Google settlement. Final court approval not confirmed in sources. External offers and alternative billing also available |

For Mudcroft at small scale, assume **about 15% (Apple Small Business) and about 15% (Google: 10% service fee plus 5% billing fee)**.

### Mechanics of consumable IAP

- **Apple:** StoreKit 2 returns a JWS-signed `Transaction`. Set `appAccountToken` (a UUID) at purchase time to bind the transaction to a (universe, account) pair. Verify on the server with the App Store Server API or App Store Server Library. App Store Server Notifications V2 send JWS-signed `signedPayload`s for `REFUND`, `CONSUMPTION_REQUEST` and similar events. Answering `CONSUMPTION_REQUEST` with consumption data informs Apple's refund decision but does not control it. `REFUND` sometimes arrives without a prior `CONSUMPTION_REQUEST`. [Apple docs, dev forums]
- **Google:** verify with the Play Developer API (`purchases.products.get`), then acknowledge or consume. If you don't acknowledge within 3 days, the purchase is refunded automatically. Real-time Developer Notifications arrive via Cloud Pub/Sub, including voided-purchase notifications (`productType` = one-time, `refundType`). The Voided Purchases API lists refunds, chargebacks, and developer- or Google-initiated voids. [Android docs]
- **Payout timing:** Apple's official wording is "within 45 days following the first Friday of each fiscal month", observed in practice as **about 33 days after the fiscal month ends**. So a purchase at the start of a fiscal month is paid about 60 or more days later. Google pays **on the 15th for the previous calendar month** (+2 to 3 bank days), so 15 to 48 days after purchase. [Apple; RevenueCat; Google Payments Center]
- **Tax and currency:** both stores act as merchant of record for the player transaction. They collect and remit sales tax and VAT in most jurisdictions, and pay developers proceeds net of commission and tax, converted into the bank account's currency. Mudcroft therefore receives clean USD, which is exactly the input a USD-to-USDC on-ramp needs.

## 2. Blockchain-side facts

- **Base:** native Circle USDC and CCTP. Supported by Circle Mint, Bridge (Stripe), and Coinbase. Gas costs are cents. The obvious settlement chain.
- **Ethereum L1:** native USDC. Gas is acceptable for monthly batches, not for per-purchase transfers.
- **Hemi:** an EVM chain (chain ID 43111, ETH gas) whose hVM embeds a Bitcoin node. Mainnet since March 2025. Hemi's docs say its native Tunnel does **not** yet support USDC or USDT and point to third-party bridges. The USDC on Hemi is **Stargate-bridged USDC.e** (Circle Bridged USDC Standard, not Circle-issued, no CCTP), with thin liquidity (about $3M market cap per one tracker). Hemi does not appear on Circle's native USDC list (as of 2026-11; recheck). Hemi tooling is standard EVM (RPC, explorer, dRPC/Infura) but stablecoin infrastructure is immature. The practical approach: settle in native USDC on Base, and deliver to Hemi via Stargate only for owners who insist.
- **Fiat to USDC rails for the operator (business-to-business, low cost):** Circle Mint (wire USD in, mint USDC 1:1, redeem 1:1). Bridge virtual accounts (ACH or wire in, auto-convert, deliver USDC to a Base address; `developer_fee_percent` supported). Stripe Connect stablecoin payouts (private preview, US platforms only, payouts to individuals and sole proprietors only, not companies, as of 2026-11). Coinbase Business.
- **Consumer on-ramps** (Coinbase Onramp, Crossmint, MoonPay, Transak) solve a different problem: players buying crypto. They are web or card flows and do not help an IAP-only app. Coinbase's hosted guest checkout ended 2026-06-30. Its headless Apple Pay path is US-only with low default limits. Crossmint's token checkout is unavailable in the EEA, UK, and Brazil since 2026-07-01.

### Regulatory framing (practical, not legal advice)

- **What makes someone a money transmitter (US):** FinCEN treats anyone who accepts value from one person and transmits it to another, or who issues *and redeems* convertible virtual currency, as a money transmitter unless an exemption applies (2013 and 2019 guidance). Game currencies stay outside that definition when they are **closed-loop**: usable only in-app, not transferable between users for value, and not redeemable for cash or crypto (the Venable/WSGR line of analysis). States add their own licensing regimes.
- **Mudcroft's one-way credits fit the closed-loop model:** players cannot withdraw. What carries the risk is the *owner payout leg*. The safest structure is for Mudcroft to act as principal: it sells its own credits and pays owners a revenue share for providing content, with its own funds, like Roblox DevEx. The riskier structure is Mudcroft "collecting on behalf of" owners. That reads as payment facilitation and needs an agent-of-payee exemption, which varies by state.
- **Never** let credits move back out as USDC to players, or be traded between players for value. That turns closed-loop credits into open-loop stored value.
- **Stablecoins:** the US GENIUS Act regulates *issuers*. The OCC final rule takes effect 2027-01-18, and FinCEN/OFAC AML rules were proposed separately and are not yet final (as of 2026-11). Holding and paying with USDC does not make Mudcroft an issuer, but its counterparties (Circle, Bridge) will be GENIUS-regulated. In the EU, MiCA treats USDC as an authorized e-money token (Circle has MiCA authorization, and USDT was delisted from EU venues in 2024-25). Paying EU owners in USDC is feasible. Custody or exchange services for others would need a crypto-asset service provider license, so Mudcroft should only ever send USDC to wallets owners control, never hold it for them.

## 3. Prior art

| Precedent | What it shows | Relevance |
|---|---|---|
| **Roblox Robux + DevEx** | Players buy a closed-loop currency via IAP. Creators earn it and the platform redeems it for cash ($0.0038/Robux since 2025-09; a 42% boost for qualifying US 18+ spend from 2026-06) | The exact economic shape of Solution A: the platform is principal and creators are paid a revenue share in arrears |
| **Telegram Stars** | Bought via Apple and Google IAP. Creators withdraw to TON via Fragment after a **21-day hold**. Buyers cannot cash out | Proof that "IAP in, crypto out to the *seller*" survives app review when the buyer side stays closed-loop. The hold works as a refund reserve |
| **Immutable mobile / Gods Unchained** | Mobile companion app using store IAP; mobile division launched in 2025 | Web3 studios keep the IAP side plain, with chain activity off to the side |
| **Stripe/Bridge, Circle Mint** | "Treasury converts fiat revenue to USDC" products exist off the shelf | The conversion and payout leg is a solved, purchasable commodity |
| **Revenue-based financing for app revenue** (receivable factoring) | Advances against store payouts are an established financing category | Template for Solution C's prefunding pool |

**Novel versus existing.** Not novel: the platform as principal with creator payouts in arrears, and fiat-to-USDC treasury payouts. Novel to my knowledge (no product found): (1) an **on-chain settlement manifest** that ties each USDC batch to the individual verified IAP transactions it pays for; (2) a **receipt-attestation receivables contract** that turns store-signed transactions into on-chain claims that can be netted against refunds.

## 4. Candidate solutions (ranked)

### A. Credit now, settle on payout day (recommended)

**Summary.** Mudcroft publishes one iOS and one Android app and is the store developer of record. Players buy consumable "Credit Pack" products. The Mudcroft payment service verifies each purchase and immediately tells the target universe's `mudd` instance to grant credits. Each purchase is booked as a payable to that universe's owner: net proceeds minus a Mudcroft platform fee minus a refund reserve. When Apple or Google pays, Mudcroft converts the USD to USDC (Circle Mint or Bridge). It then sends **one USDC transfer per universe vendor wallet on Base**, plus a `SettlementPosted(universe, period, merkleRoot, total)` event. Each Merkle leaf is `(storeTxId, accountId, credits, netUsd)`, so every "credit user foo N credits" line is provable on-chain after the fact.

**Flow.**
1. The player picks a pack in the app. The app shows "1,000 credits in *universe-x*" (the quantity comes from universe config). The app calls StoreKit or Play Billing with `appAccountToken`/`obfuscatedAccountId` = hash(universe, account). *Custody: Apple or Google.*
2. The store charges the player. The app sends the JWS transaction (Apple) or purchase token (Google) to the Mudcroft payment service. The service verifies with the store API, records the transaction idempotently, and consumes or acknowledges it. Takes seconds.
3. The payment service calls the instance's grant endpoint, signed with a key the instance trusts (an `mudd` admin-signed grant into the existing `CreditManager::grant`, keyed on `storeTxId`). The player sees the credits immediately. *No money has moved. Mudcroft owes the owner a payable.*
4. Refunds: an ASSN V2 `REFUND` or Google voided-purchase event triggers a negative ledger entry against the owner's payable. The instance may claw back unspent credits (policy per universe).
5. Payout day (Google on the 15th, Apple about 33 days after fiscal month end): USD lands in Mudcroft's bank. *Custody: Mudcroft, in fiat.*
6. Mudcroft wires the USD to Circle Mint (or a Bridge virtual account) and receives USDC on Base. *Custody: Mudcroft, in USDC, for hours.*
7. A batch transaction sends each owner's net amount. The current reserve is held back (for example, 10% held for 90 days, released on a rolling basis). The settlement event is emitted. The instance's chain watcher reconciles the event against its local grant log and flags mismatches. *Custody: universe owner.*

**Cost and capital.** Float: **$0**. Owners are paid from money already received, and the reserve is their own money held back. Per transaction: store fee (about 15%), Mudcroft fee (policy), USDC conversion (Circle Mint is close to 0, Bridge is in basis points; get quotes), and Base gas (cents per batch). Fixed costs: a business bank account and a Circle Mint or Bridge business account (KYB required).

**User experience.** Players see a normal IAP sheet and instant credits, with no wallet. Owners see a USDC deposit each month and a dashboard of the transactions behind it. The settlement delay for owners is 2 to 9 weeks.

**Compliance and app-store risk.** Low on the store side: a plain consumable, no crypto in the app. On the money-transmission side, Mudcroft acts as **principal**. It sells its own product (credits redeemable only in Mudcroft universes) and pays vendors a revenue share for content or services with its own funds, like Roblox. It does not accept money *for transmission* to the owner. Avoid letting players or owners send credits back out as value. Needs owner KYC (W-9/W-8), 1099 reporting at USD fair market value (the 2026 threshold may have changed; verify), OFAC screening of wallet addresses, and terms of service that treat the revenue share as a payment for services.

**What to build (about 4 to 6 engineer-weeks).** Mobile purchase UI (React Native/Capacitor wrapper around the existing React client, or native StoreKit/Billing modules). A payment-service crate: App Store Server API and ASSN V2 verification (JWS x5c chain check), Play Developer API, Pub/Sub RTDN ingest, an idempotent ledger. An `mudd` signed-grant endpoint and a chain watcher. A settlement job: Merkle build, Circle/Bridge API calls, Base batch send via a multisig or MPC hot wallet. An owner dashboard.

**Why it might fail.** Owners may want money sooner; mitigated by Solution C. Refund abuse on consumables (Apple decides refunds); mitigated by the reserve and by sending consumption data. A state regulator might view the revenue share as transmission; get counsel early. App Review may object to server-determined pack contents that vary by universe; mitigated by fixed tiers and clear in-app disclosure.

### B. Signed-receipt attestation ledger (receivables on-chain)

**Summary.** Same commercial structure as A, but each verified purchase is posted on-chain **in near real time** as an attestation to a `MudcroftReceivables` contract on Base (or Hemi). This is the literal "message meaning credit user foo N credits", arriving at the instance within seconds, with **no USDC attached yet**. The contract accrues a per-universe receivable. On payout day Mudcroft deposits USDC and owners call `claim()`. Refunds are posted as negative attestations and netted automatically. Instances credit players by watching contract events rather than trusting a Mudcroft HTTP call.

**Flow.** Steps 1 and 2 as in A. Step 3: the relayer submits `attest(universe, accountHash, credits, netUsdMicros, storeTxIdHash, sig)`, signed by Mudcroft's attestor key. Optionally it also includes the raw Apple JWS for on-chain ES256 verification: Base has the P-256 precompile, and Ethereum added one in Fusaka (2025-12, unverified). The instance's watcher grants credits on the event (about 2 to 10 s). Step 4: refunds become `void(storeTxIdHash)`. Step 5: payout day `fund(universe, amount)` in USDC, then owners `claim()`. *Custody:* store, then Mudcroft (fiat), then the contract (USDC, briefly), then the owner.

**Cost and capital.** Float is still $0. Gas: one attestation per purchase (sub-cent on Base) or batched every N minutes. Requires a smart-contract audit (roughly $10k to $40k).

**User experience.** Identical for players. Owners get a live, verifiable revenue feed and an on-chain receivable they could borrow against, which is what makes Solution C pluggable.

**Compliance and app-store risk.** As A. Hashing account IDs keeps personal data off-chain. Publishing per-purchase revenue publicly may leak business data; owners must accept that.

**What to build.** Everything in A, plus the contract, relayer, and audit: about **+3 to 5 weeks**.

**Why it might fail.** It is genuinely trust-minimized only for Apple *purchases*. Google's portable signature (the RSA-signed `originalJson`) and both platforms' *refunds* still depend on Mudcroft as the oracle. Apple's certificate chain uses P-384 at higher levels (unverified), so full on-chain chain verification is costly; you would pin rotating leaf keys instead. It is also complexity that small owners may not value.

### C. Prefunded instant USDC ("advance pool")

**Summary.** On each verified purchase, a USDC pool immediately sends the owner's net amount (minus reserve) to the vendor wallet, together with the credit message. This is literally "USDC plus credit user foo" in real time. The pool is refilled on payout day. It can be funded by Mudcroft's own capital, a revenue-based lender, or outside liquidity providers earning a discount fee (for example 1 to 2% per advance). It is best offered as an **opt-in "instant settlement" tier** on top of A or B.

**Flow.** Purchase verified, then the pool contract `advance(universe, amount, creditMsg)` transfers USDC (seconds), then the instance grants credits. A refund before payout becomes a debt against the owner's future advances or reserve. Payout day: USD converted to USDC, which repays the pool. *Custody:* the pool (USDC) goes to the owner immediately. The pool is exposed to store payment risk for 15 to 63 days.

**Cost and capital.** Capital needed is about the peak outstanding receivable: **roughly 2 months of net owner payouts, plus refund headroom**. For example, $20k/month of gross IAP comes to about $17k net, so about **$35 to 40k** of pool capital. Cost of capital or LP yield is passed to owners as an advance fee.

**User experience.** Best for owners (instant money). Same for players.

**Compliance and app-store risk.** Highest of the three. Third-party liquidity providers make this a lending product (state lending and securities questions). Fraud exposure: stolen-card IAP purchases are refunded weeks after the USDC has already left.

**What to build.** A or B, plus a pool contract, risk limits (per-universe caps, velocity limits, account-age gates), and lender agreements: **+4 to 8 weeks**, plus legal work.

**Why it might fail.** It needs capital the project lead wants to avoid. Fraud losses are not recoverable once the USDC has left. Regulatory overhead outweighs the benefit at small scale.

### Ideas considered and folded in or rejected

- *Netting and batching on payout day*: this is the core of A.
- *Operator as merchant of record settling in USDC*: also A.
- *On-chain receipt oracle with a claim contract*: this is B.
- *Credit line or "credit now, settle later" with chargeback reserve*: C, plus the reserve in A.
- *Stripe stablecoin payouts*: a viable alternative leg 6 for individual owners only (preview, US platforms).
- *Minting credits on-chain* (credits as tokens): rejected. It would make the IAP item a tokenized digital asset (Google declaration, Apple 3.1.1 scrutiny) and pushes toward open-loop stored value. Keep credits off-chain and closed-loop. Put only the dollars and the accounting on-chain.
- *US link-out to a web crypto on-ramp*: outside this question's constraint (the app must use store payments only). It is a cheap parallel lane for US desktop and web players.

## 5. Comparison

| | A. Settle on payout day | B. Attestation ledger | C. Prefunded instant |
|---|---|---|---|
| Float / capital | $0 | $0 (+ audit) | about 2 months of net payouts |
| Owner sees USDC | 15 to 63 days | 15 to 63 days (receivable visible in seconds) | seconds |
| On-chain credit message | batched manifest | real time | real time |
| Player sees a wallet | never | never | never |
| App-store risk | low | low | low |
| Regulatory risk | low to medium | low to medium | medium to high |
| Build effort | about 4 to 6 weeks | +3 to 5 weeks | +4 to 8 weeks + legal |
| Novelty | low (Roblox/Telegram pattern) | medium to high | medium |
| Likelihood of success | highest | high if owners want verifiability | niche |

## 6. Recommended path

1. **Build A with B's data model.** Make the off-chain ledger record the exact leaf format a future attestation contract would use. Upgrading to B then means adding a relayer, not a migration.
2. **Settle on Base in native USDC** through Circle Mint (lowest cost at volume) or Bridge (faster onboarding, API-first). Offer Hemi delivery via Stargate USDC.e as an opt-in, with bridge fees passed through.
3. **Refund reserve:** 10% held for 90 days to start, tuned from observed refund rates.
4. **Get a money-transmission and tax opinion** on the "principal plus revenue-share" structure before the first live payout.

**Minimal first experiment (about 1 to 2 weeks, no real money):**
- An iOS sandbox and a Play internal test track app with one consumable product.
- A small Rust service that verifies a StoreKit 2 JWS and a Play purchase token, ingests sandbox ASSN V2 and RTDN refunds, and calls `CreditManager::grant` on a dev `mudd` with idempotency keyed on the store transaction ID.
- A script that builds the month's Merkle manifest and sends test USDC on **Base Sepolia** to a vendor wallet with a `SettlementPosted` event. A watcher in `mudd` reconciles it against local grants.
- Success criteria: grant latency under 5 s, a refund correctly reverses and nets, the manifest proves every grant, and no wallet UI on device.

## 7. Open questions

- Is Mudcroft willing to be the single store developer of record for all universes, and to take on UGC moderation duties (Apple 1.2) for third-party-hosted universes?
- Will App Review accept fixed-price packs whose credit quantity varies by universe? Or do we need per-universe product IDs (which doesn't scale)?
- Who bears consumable refund losses after credits are spent: the owner via the reserve, or Mudcroft?
- Will owners accept net-30 to net-60 settlement? Is a paid instant tier (C) worth its capital?
- Does a revenue share paid in USDC create state money-transmitter or 1099/withholding issues for non-US owners? (Counsel.)
- Supreme Court disposition of Apple's petition (No. 25-1311) and the final US link-out rate; final court approval of Google's US fee model.
- Will Circle issue native USDC on Hemi, or will Hemi's Tunnel add USDC? Either would change the "Base first" recommendation.

## 8. Sources

- Apple App Review Guidelines (3.1.1, 3.1.1(a), 3.1.3, 3.1.5): https://developer.apple.com/app-store/review/guidelines/
- Apple dev forums, IAP still required after US changes: https://developer.apple.com/forums/thread/844046
- MacRumors, US guideline update (2025-05-01): https://www.macrumors.com/2025/05/01/apple-updates-u-s-app-review-guidelines-epic/
- MacRumors, Ninth Circuit allows a commission (2025-12-11): https://www.macrumors.com/2025/12/11/apple-app-store-fees-external-payment-links/
- MacRumors, stay reversed (2026-04-29): https://www.macrumors.com/2026/04/29/epic-games-wins-reversal-app-store-fee-battle/
- Supreme Court docket 25-1311: https://www.supremecourt.gov/docket/docketfiles/html/public/25-1311.html
- MLex, Apple 15% remand proposal: https://www.mlex.com/mlex/articles/2513410
- Apple EU terms (2026-08): https://www.apple.com/newsroom/2026/08/apple-announces-changes-for-apps-in-the-european-union/ (mirror: https://www.apple.com/ca/newsroom/2026/08/apple-announces-changes-for-apps-in-the-european-union/)
- Apple Japan MSCA changes (2025-12-17): https://www.apple.com/newsroom/2025/12/apple-announces-changes-to-ios-in-japan/
- Google Play blockchain-based content policy: https://support.google.com/googleplay/android-developer/answer/13607354
- Google, Epic settlement / new fees: https://android-developers.googleblog.com/2026/03/a-new-era-for-choice-and-openness.html ; https://android-developers.googleblog.com/2026/06/play-expanded-billing.html ; https://techcrunch.com/2026/03/04/google-settles-with-epic-games-drops-its-play-store-commissions-to-20/
- Apple send consumption information: https://developer.apple.com/documentation/appstoreserverapi/send-consumption-information
- Apple dev forums, REFUND without CONSUMPTION_REQUEST: https://developer.apple.com/forums/thread/785315
- Google RTDN reference: https://developer.android.com/google/play/billing/rtdn-reference
- Google Voided Purchases API: https://developers.google.com/android-publisher/voided-purchases
- Apple Payments and Financial Reports: https://itunespartner.apple.com/music/support/5243-getting-paid
- RevenueCat, Apple fiscal calendar and payment dates: https://www.revenuecat.com/blog/app-store-fiscal-calendar-and-payment-dates
- Google Payments Center payout timing: https://support.google.com/paymentscenter/answer/7159355
- Hemi network details: https://docs.hemi.xyz/discover/network-details
- Hemi, tunnel ERC-20s via third party (USDC not in native Tunnel): https://docs.hemi.xyz/how-to-tutorials/using-hemi/tunnel-from-ethereum/tunnel-erc20s-via-3rd-party
- Stargate-bridged USDC on Hemi: https://beincrypto.com/price/stargate-bridged-usdc-hemi/
- DefiLlama Hemi stablecoins: https://defillama.com/stablecoins/Hemi
- Circle USDC contract addresses: https://developers.circle.com/stablecoins/usdc-contract-addresses
- Circle CCTP: https://www.circle.com/cross-chain-transfer-protocol
- Circle, USDC vs USDC.e: https://www.usdc.com/learn/whats-the-difference-between-usdc-and-usdce
- Stripe stablecoin payouts for Connect: https://docs.stripe.com/connect/stablecoin-payouts
- Stripe stablecoin financial accounts: https://docs.stripe.com/crypto/stablecoin-financial-accounts
- Bridge virtual accounts: https://apidocs.bridge.xyz/api-reference/virtual-accounts/create-a-virtual-account
- Coinbase Onramp FAQ / headless onramp: https://docs.cdp.coinbase.com/onramp/additional-resources/faq ; https://docs.cdp.coinbase.com/onramp/headless-onramp/overview
- Crossmint onramp and checkout: https://docs.crossmint.com/onramp/ ; https://www.crossmint.com/products/checkout
- Immutable mobile / Gods Unchained: https://gamesbeat.com/immutable-games-launches-gods-unchained-web3-game-on-mobile/
- Telegram Stars ToS and creator terms: https://telegram.org/tos/stars ; https://telegram.org/tos/content-creator-rewards
- Roblox DevEx rate (2025-09): https://www.businesswire.com/news/home/20250905587466/en ; 2026 boost: https://about.roblox.com/newsroom/2026/04/roblox-fuels-high-fidelity-games-over-18-players-increases-qualifying-devex-rate-42
- FinCEN 2013 virtual currency guidance: https://www.fincen.gov/statutes_regs/guidance/pdf/FIN-2013-G001.pdf
- FinCEN 2019 CVC guidance: https://www.fincen.gov/sites/default/files/2019-05/FinCEN%20Guidance%20CVC%20FINAL%20508.pdf
- WSGR, gaming companies as inadvertent MSBs: https://wsgr.com/en/insights/how-gaming-companies-can-become-inadvertent-money-services-businesses.html
- OCC GENIUS Act final rule (effective 2027-01-18): https://occ.gov/news-issuances/news-releases/2026/nr-occ-2026-9.html ; https://www.sheppard.com/insights/blogs/occ-finalizes-genius-act-rules-for-payment-stablecoin-issuers
- MiCA and USDC/USDT in the EU: https://beincrypto.com/circle-mica-winner-usdc-eurc-usdt/ ; https://www.scorechain.com/blog/eu-stablecoin-regulation-mica
