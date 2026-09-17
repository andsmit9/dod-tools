# Demo stats probes

Measurement tools used to establish what match statistics can and cannot be
recovered from a Day of Defeat 1.3 demo file. They exist so the findings below
are reproducible rather than asserted.

These are `examples/`, so they are not part of any shipped binary. `cargo build`
ignores them; `cargo build --examples` and `cargo test` compile them.

## The probes

| Probe | Question it answers |
| --- | --- |
| `msg_probe` | What messages are on the wire, and how wide is each one? |
| `scoreboard_probe` | How much of a scoreboard can one demo produce? (exploratory, naive) |
| `batch_probe` | One TSV row of coverage metrics per demo, for corpus-wide sweeps |
| `hltv_probe` | Is this a true HLTV recording or a POV demo carrying director frames? |
| `reconcile_probe` | Do derived kill counts agree with the server's own frag counter? |
| `reconnect_probe` | What does a reconnect do to the server's score counters? |
| `capwindow_probe` | How far is a flag capture from the objective-score credits it earned? |
| `weapon_switch_probe` | Where does a player rapidly cycle weapons, on the demo's own clock? |

`weapon_switch_probe` exists because `goldsrc-hooks`' log cannot answer "where
in the demo was that?". Its clock counts from when the *client* loaded and
keeps running while playback is paused, so it has no fixed relationship to a
position in the file. This reads `entity_state_t::weaponmodel` — the same
replicated field the animation fix reads — off `frame.time`, which is the
demo's own, and prints bursts of rapid switching. Matching a burst's shape
against the log is how the two clocks get lined up; see the module doc.

Run any of them against a single demo:

    cargo run --release -p analysis --example msg_probe -- path/to/demo.dem

`batch_probe` is designed for sweeps. Drive it one process per demo so a file
that panics the parser cannot take down the run:

    for f in /path/to/demos/*.dem; do
      timeout 120 ./target/release/examples/batch_probe "$f" >> out.tsv
    done

## What they established

Measured across 624 demos — a mixed POV library plus 126 LAN HLTV recordings.

- **No headshots, ever.** `DeathMsg` was exactly 3 bytes in all 211,335
  instances. There is no hit group in the message, so headshot and
  headshot-rate columns cannot come from a demo.
- **No damage, and no assists.** Every one of the 126 true HLTV demos carried
  exactly one `Health` message — the proxy's own slot. POV demos average 168,
  but only for the recording player. Assists need damage attribution, so they
  fall with it.
- **Flag-capture credit needs two messages.** `CapMsg` named exactly one capper
  in all 26,476 captures, never two. Co-cappers appear as `ObjScore` increments
  in the *same frame*, 99.5% of the time, so no tolerance window is needed.
  About 20% of captures had at least one co-capper.
- **Kill counts reconcile, once resets are handled.** Comparing a reset-aware
  derived count against the server's own frag counter over 6,567 player rows:
  75.3% agree exactly, 89.8% within one kill. Counting naively, without
  handling the match-start scoreboard wipe, drops that to 15%.
- **Reconnects restart the server's counters.** Reading the last value seen
  undercounts by 0.64% overall but loses up to 94 kills in a single demo;
  reading the highest value overcounts by 8.5%, because the peak predates the
  match-start wipe. Derived counts keyed on SteamID are immune to both.
- **`SvcDirector` is not an HLTV marker.** It appears in ordinary POV demos
  whenever an HLTV caster is spectating, and demo patchers inject it. `SvcHltv`
  is the reliable signal.
- **About 1% of demos will not parse.** Plan for a per-file failure path.
