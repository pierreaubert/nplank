# Top bar: where next + provider usage

For the 5-workflows / 20-30-tools / 4-machines case, the title bar
answers two questions without opening anything:

- **Where do I go next?** A chip `⚑ N need you · <name> (<reason>)`
  appears when sessions wait on the user. Clicking it — or
  `Cmd+Shift+J`, or View → Next Needs Attention — jumps to the next
  session in Fleet attention order (approval-blocked first, then
  recovery, failure, disconnected, generic attention), cycling.
  The ordering lives in `AttentionReason::rank` (`src/fleet.rs`) and
  `src/usage.rs`, both unit-tested.
- **How much am I using per provider?** Chips beside the CPU/GPU meters
  show `Provider ▶running · tokens` aggregated from live session
  telemetry (`generated` counters), with `· ctx NN%` added when a
  provider's peak context fill reaches 75%.

No provider exposes a quota ("weekly left") API to the daemon, so the
numbers are derived usage, not quota. If a provider later exposes real
quota, `provider_usage` in `src/usage.rs` is the single place to swap
the source while keeping the presentation.
