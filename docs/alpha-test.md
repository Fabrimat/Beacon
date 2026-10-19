# Beacon alpha test: install to Play

This is the script for the two Phase 5 exit tests. Both are **OPEN** until a person has run them.

1. A clean Windows 11 machine goes from install to Play in under 5 minutes, with no other installs.
2. A non-coder completes the flow unaided.

The tester follows "Tester steps". The observer prepares the machine, keeps time and writes down
what happens. The observer does not help (see "Passing").

## Observer: before the test

- **Windows 11**, clean (no Node, Python or VS Code needed or wanted). WebView2 is preinstalled
  on Windows 11. Windows 10 is not gating.
- **Minecraft Bedrock** from the Microsoft Store, **launched once, signed in, and left at the main
  menu**, then closed or left open. If it was never launched, Beacon shows `BCN0200` ("No
  Minecraft install found") because the game's data folders do not exist yet.
- Optional: Minecraft Education. Beacon picks the stable install of the project's edition; the
  default template targets Bedrock.
- Minecraft updated to the current release (the template's script needs the current
  `@minecraft/server`).
- Internet is not needed for the happy path: nothing downloads on Continue (the API index and
  BDS are optional and unchecked).
- **Installer delivery.** The tester must download the installer **in the browser, from a link**,
  so Windows marks it as downloaded from the internet (Mark-of-the-Web) and the SmartScreen step
  below is the real one. Do not copy it over from a zip, a USB stick or a share that strips the
  mark, and do not extract it with a tool that drops it.
  - The GitHub Actions artifact (`desktop.yml`, "windows (NSIS, unsigned)") needs a GitHub login
    and arrives as a zip, so it is not suitable for the tester. Prepare it yourself: download the
    artifact, unzip it, and upload the `.exe` to a file share or any plain download link (a
    direct link to the `.exe`). The tester opens that link in Edge/Chrome and clicks it.
  - Nothing is published (no release, no store), so this is the only delivery path.
- Note the installer file name and the build (commit) you are testing.

## Tester steps

Start the stopwatch when the tester clicks the download link. Step names match the automated
install smoke so the two can be compared.

1. **Install.**
   1. Open the link and download the installer (`Beacon_..._x64-setup.exe`). Open it from the
      browser's downloads.
   2. Windows shows **"Windows protected your PC"** (SmartScreen), because the alpha is not
      signed. Click **More info**, then **Run anyway**.
   3. Windows Defender may also ask or scan the file. If it asks, choose to allow/continue; a
      scan that takes a few seconds is normal.
   4. The installer needs no administrator password. On the folder page it may show
      `%LOCALAPPDATA%\Beacon`, while the files really go to `%LOCALAPPDATA%\Programs\Beacon`
      (a known cosmetic issue). Do not change anything: click through **Next**/**Install**, then
      **Finish** (leave "Run Beacon" ticked).
2. **First launch.** Beacon opens on the **Setup** screen. Under **Minecraft** it lists the
   install it found. Leave everything as it is (the optional tools stay unchecked, in particular
   the Bedrock Dedicated Server) and click **Continue**. Wait until the **Projects** screen (Home)
   appears.
3. **Project.** On Home, under **New project**, type a **Folder** that does not exist yet or is
   empty (for example `C:\Users\<you>\my-addon`; the name defaults to the folder name) and click
   **Create**. The project opens and shows "Trusted."
4. **Trust (only if asked).** A project created by Beacon is already trusted, so you normally do
   not see this. If **"Trust this project?"** appears, it means: Beacon is about to run the
   scripts that come with this project on your computer, so only continue for a project you made or
   got from someone you trust. Click **Trust and continue**.
5. **Play / deploy.** Click **Play** in the left menu, then the **Play** button. It shows
   "Building..." and then **"Deployed. Minecraft is starting."** (If it says "Deployed. Start
   Minecraft to try it.", start Minecraft yourself.)
6. **Game launch.** Minecraft opens. Wait for the main menu.
7. **First in-game check.** Beacon created a world for the project, with the packs already on.
   Nothing to activate:
   1. **Play** > **Worlds** and open the world named after your project (the Name you typed).
      It is a flat creative world with cheats on.
   2. Wait for it to load, then look at the chat. You should see the line
      **`<your namespace> loaded`**, where the namespace is the project name in lowercase with
      letters, digits and `_` (project `my-addon` prints `my_addon loaded`). That message is
      sent by the template's script and is the pass signal. Stop the stopwatch.

> Dev packs and worlds: Play puts the packs into the game's `development_behavior_packs` /
> `development_resource_packs` folders and, on the first Play, creates the project's managed dev
> world in `minecraftWorlds` (ARCHITECTURE 4.3) with both packs listed. Later Plays only refresh
> that pack list. If the world is missing or the chat line does not appear, record it. The exact
> wording of the in-game menus may differ by game version; the observer records any difference.

### Optional: see the game's log in Beacon

In Minecraft: **Settings** > **Creator** > **Enable Content Log File**. With it on, the Play
screen's **Content log** box fills with the game's messages after a Play. With it off (the
default) the box stays on "Nothing yet." and the test is unaffected.

## Stopwatch

Record the clock time at the start and end of each step. Target: **under 5:00 total** (download
click to the chat message). Prompts count (SmartScreen is part of the 5 minutes).

| Step | Start | End | Seconds | Notes |
|---|---|---|---|---|
| install (download, SmartScreen, installer) | | | | |
| first launch (app opens, Setup, Continue, Home) | | | | |
| setup (Continue until Home) | | | | |
| project (Create) | | | | |
| Play/deploy (Play click to "Deployed") | | | | |
| game launch (until main menu) | | | | |
| first in-game check (open the project world, chat message) | | | | |
| **Total** | | | | |

First launch and setup overlap: time "first launch" from double-click to the Setup screen
appearing, and "setup" from the Setup screen to Home.

## Observer: during and after

Do not help, hint or point at the screen. If the tester is stuck for more than 2 minutes, or asks
for help, write down where and what you said, then give the smallest possible nudge and mark the
run as "helped".

Stuck-point questions to ask afterwards:

- Where did you hesitate or not know what to do next?
- Was any text confusing (SmartScreen, the folder page, Setup, the trust question)?
- Did you expect something to happen that did not (a world opening by itself, for instance)?
- Did anything look broken or scary?
- What would you change first?

Did you need help? Where? (answer for every step in the table above)

### Severity

| Level | Meaning |
|---|---|
| S0 | Cosmetic; no effect on the flow (for example the install folder shown on the installer page). |
| S1 | Hesitation or a wrong click, recovered alone within a minute. |
| S2 | Needed a nudge from the observer, or took over 2 minutes on one step. |
| S3 | Could not continue; failed the step. |

### Passing

- **Exit 1 (time)**: machine clean, total under 5:00, every step done. Run it on a real clean
  Windows 11 with the game; the automated smoke covers only the part up to packs deployed.
- **Exit 2 (non-coder)**: the tester matches the non-coder persona (ARCHITECTURE 2.3: designer or
  educator, not a programmer), completed steps 1 to 7 with **no help** and no S2 or S3. Any help
  needed = not passed; record why. The unsigned SmartScreen step counts: if it alone stops the
  tester, that is the result (signing is the fix, deferred by decision).

## Results

Copy this block per run.

```
Date:
Observer:
Tester (role, not name):
Machine (Windows build, Minecraft version, edition):
Installer file / build:
Browser used for download:

Timings: install __  first launch __  setup __  project __  Play/deploy __  game launch __  first in-game check __
Total: __ (target < 5:00)

Passed exit 1 (time, clean machine)? yes / no
Passed exit 2 (unaided)?             yes / no

Help needed (step, what was said):
Stuck points / quotes:
Issues (S0-S3, step, description):
Defender / SmartScreen behaviour seen:
Content log tried? yes / no
```
