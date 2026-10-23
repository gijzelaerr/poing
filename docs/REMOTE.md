# Poing on the Mac, the work on the Windows PC

Poing can run its UI on one machine and its GPU jobs on another. The setup we use is the UI on Gijs's Mac and the jobs on the Windows PC with the RTX 4070 Ti. The jobs that move are analysis, Describe takes, de-reverb, new parts, remixes and singing a new song. Playback, the mix, plugins, Render and Claude's writers (MIDI parts, effects, a new song's sheet) stay on the Mac.

The PC runs **poing-node**. It works like a Makepad AI hub node:
- a self-signed TLS certificate, which the Mac records on first contact (`~/.makepad/ai-hub/known_nodes`);
- the hub's TLS front, which checks a per-machine fleet credential on every request;
- a LAN beacon in fleet `poing`.

Before a job, the Mac sends the node the files it needs and does not have yet, matched by sha256. Each song gets a mirror on the node in `bundles/<machine>/<bundle id>/`. While the job runs, the node streams progress. Before each result it lists the files the job changed, and the Mac downloads them into the song's own folder, then decodes stems and takes locally. Each file travels once.

## On the Windows PC

```sh
cargo build --release -p poing --bins          # poing.exe and poing-node.exe (poing-engine as in CLAUDE.md)
target/release/poing-node pair mac             # prints a credential and a `poing-node join …` line per address
target/release/poing-node                      # serves on port 8125 (--port N, --listen ADDR, --no-beacon)
```

- The node runs jobs exactly as Poing does on this PC, with the same models, `uv` runtimes, `POING_ACESTEP`, ffmpeg and `poing-engine` beside the exe. Whatever works in Poing on the PC works through the node.
- Only one job runs at a time, because the GPU is shared. A second job waits, and its status line says so.
- The node's files live in `%LOCALAPPDATA%\Poing\node`, or `POING_NODE_HOME`:
  - `tls/` holds its certificate;
  - `fleet/fleet.key` holds the key that issues credentials;
  - `fleet/revoked` lists revoked credentials;
  - `bundles/` holds the song mirrors, safe to delete.
- Programs the node starts end with it, because they are in its Windows job object. Killing the node does not leave ACE-Step holding the GPU.
- Windows Firewall: the first start asks whether to allow poing-node on private networks; say yes. Without the prompt, add a rule yourself (admin PowerShell):

  ```powershell
  New-NetFirewallRule -DisplayName "poing-node" -Direction Inbound -Protocol TCP -LocalPort 8125 -Action Allow -Profile Private
  ```

  The beacon is outgoing UDP broadcast on port 41830, so it needs no rule on the PC.
- Revoke a machine with `poing-node revoke mac`, then restart the node.

## On the Mac

1. Build Poing. CI builds it on macOS, and its `poing-macOS-arm64` artifact has `poing` and `poing-node`. From a checkout:

   ```sh
   cargo build --release -p poing --bins
   ```

2. Find the PC if you don't know its address. Run `target/release/poing-node find`, which lists the nodes whose beacons reach the Mac. Run on the PC itself, it may list a virtual adapter's address instead.

3. Pair it once. Paste the `join` line that `poing-node pair` printed on the PC, with the PC's LAN address. Over Tailscale use the PC's tailnet name, e.g. `glacier.gemsbok-iguana.ts.net:8125`.

   ```sh
   target/release/poing-node join mkc2.mac.lan.… 192.168.1.20:8125
   ```

   This keeps the credential in `~/.makepad/ai-hub/poing.credential` (mode 600). It also makes the PC Poing's job target, saved in `~/Library/Application Support/Poing/node.txt`, and checks that the PC answers.

4. Run Poing as usual. While a job runs, its progress in the status line ends with `· on <PC name>`. To try it without changing the saved choice:

   ```sh
   POING_NODE=192.168.1.20:8125 target/release/poing
   POING_NODE=local target/release/poing          # everything on this machine
   ```

To check that it works, open a song (analysis runs on the PC, and the lanes fill in), Describe a remix, and make a new song. Their takes play on the Mac. If the PC stops, sleeps or loses the network mid-job, the job fails within about 20 s with "The Windows PC is not reachable", and Poing carries on.

## For the Settings panel

`remote.rs` is the API:
- `target()` / `set_target(Target::Local | Target::Node("host:port"))` reads and saves the choice.
- `targets()` lists the configured node and nodes heard on the LAN. The first call starts listening.
- `status()` / `status_line()` report where jobs run, whether this machine is paired, the PC's name and the last error.
- `pair(credential)` keeps a credential.
- `check(address)` tests a node.

`POING_NODE` and `POING_NODE_CREDENTIAL` override the saved choice and the credential file.

## Limits

- Jobs are not cancelled when the Mac goes away: the node finishes the job, and its files stay in its mirror.
- A job that was cut off is not resumed. Run it again; its inputs are already on the node.
- The beacon only advertises the node. Poing never switches to a node on its own; choose one with `join`, `POING_NODE` or Settings.
