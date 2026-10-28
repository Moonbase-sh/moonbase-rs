# Security and hardening

This SDK answers one question and answers it soundly: *is this license valid, for this
product, on this machine, right now?* What it cannot do is defend the code that asks.
Validation runs inside your binary, on a machine the user controls, and the result
eventually becomes a branch. Keeping a token honest and keeping a shipped binary honest
are two different problems; the SDK solves the first and leaves the second to you,
deliberately.

## What the SDK guarantees

- **Only Moonbase can mint a license.** Every token is an RS256 JWT verified against
  the public key you embed, with the algorithm pinned. `none`, HMAC and anything else
  are refused.
- **A license is bound to your product.** The `aud` claim must contain your product id,
  and if you set an account id the issuer must match too.
- **A license is bound to one machine, and the binding is recomputed rather than read.**
  The `sig` claim is checked against a device id derived from the machine's own
  hardware on every validation, never from a file, so there is no stored value to edit.
  See [device identity](device-identity.md).
- **Offline tolerance is bounded.** The grace period caps how long an online-activated
  license runs without a successful server check. Transient failures fall back to the
  local result inside it; definitive rejections propagate immediately regardless.
- **A license covers the versions it was sold for.** One with limited updates is judged
  against `application_version` by Moonbase when it is asked and by the device clock
  when it is not, so neither the throttle nor the grace period keeps an uncovered
  version running.
- **Something in the way is not a verdict, and is not a pass either.** A captive portal,
  a proxy's block page or a misrouted 404 is treated as "no answer", so it neither locks
  out a paying customer nor extends a license past its grace period.
- **The stored file is a cache, not a credential.** Every field is re-derived from the
  signed token. Do the same: branch on what the validator returned, never on fields read
  out of the store.
- **The defaults fail closed.** Nothing is persisted unless you choose a store, and in
  the Tauri plugin `is_licensed()` is false until a license has been validated.

The Tauri plugin adds one thing: the raw token never reaches the webview. The frontend
gets the license's fields, and the plugin keeps the credential.

## What it deliberately leaves to you

There is no anti-debugging, obfuscation, integrity self-checking or tamper detection,
and there will not be. Hardening works only when it lives inside your binary and is
specific to it: anything general enough to ship in an open-source crate would be
public, identical in every app using it, and one published bypass would apply to all of
them.

## Principles

- **Gate in Rust, not in the webview.** In a Tauri app, a check in JavaScript is a
  suggestion. Put the check that counts in the command that does the paid work, as the
  [example](../examples/tauri-app/src-tauri/src/lib.rs) does.
- **Ask more than once, in more than one place.** A single call site that decides
  everything is a single thing to change.
- **Gate what the customer pays for**, not the window or the menu item.
- **Fail closed.** The failure you did not anticipate should be the safe one.
- **Separate detection from response.** Nothing requires a rejection to be immediate,
  adjacent to the check, or loud.
- **Take the free wins.** Release builds, stripped symbols, code signing.
- **Use the server you are already talking to.** Wire revocation to a real "deactivate"
  affordance so seat limits mean something.
- **Price it honestly.** The goal is raising cost, not eliminating piracy, and every
  measure is paid for by legitimate users: the studio behind a locked-down network, the
  engineer who swapped a motherboard.
