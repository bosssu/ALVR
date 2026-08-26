<p align="center"> <img width="500" src="resources/ALVR-Grey.svg"/> </p>

# ALVR - Air Light VR

[![badge-discord][]][link-discord] [![badge-matrix][]][link-matrix] [![badge-opencollective][]][link-opencollective]

Stream VR games from your PC to your headset over Wi-Fi.  
This is a fork of [ALVR](https://github.com/polygraphene/ALVR).

### Direct download (latest version):
### [Windows Launcher](https://github.com/alvr-org/ALVR/releases/latest/download/alvr_launcher_windows.zip) | [Linux Launcher](https://github.com/alvr-org/ALVR/releases/latest/download/alvr_launcher_linux.tar.gz)

## Compatibility

|          VR Headset          |                                        Support                                         |
| :--------------------------: | :------------------------------------------------------------------------------------: |
|       Apple Vision Pro       |    :heavy_check_mark: ([store link](https://apps.apple.com/app/alvr/id6479728026))     |
|      Quest 1/2/3/3S/Pro      | :heavy_check_mark: ([store link](https://www.meta.com/experiences/7674846229245715) *) |
|     Pico Neo 3/4/4 Ultra     |                                   :heavy_check_mark:                                   |
|    Play For Dream YVR 1/2/MR |                                   :heavy_check_mark:                                   |
| Vive Focus 3/Vision/XR Elite |                                   :heavy_check_mark:                                   |
|     PhoneVR (smartphone)     |     :heavy_check_mark: ** ([repo](https://github.com/PhoneVR-Developers/PhoneVR))      |
|        Android/Monado        |                                   :warning: **                                         |
|           Lynx R1            |                                   :warning: ***                                        |
|          Oculus Go           |                 :x: ([old repo](https://github.com/polygraphene/ALVR))                 |

\* ALVR for Quest 1 is not available through the Meta store.  
\** Works on some smartphones, but has not been extensively tested.
\*** Temporarily removed, last supported on version [20.14.1](https://github.com/alvr-org/ALVR/releases/tag/v20.14.1).

|     PC OS      |                                    Support                                    |
| :------------: | :---------------------------------------------------------------------------: |
| Windows 10/11  | :heavy_check_mark: ([store link](https://store.steampowered.com/app/3312710)) |
| Windows XP/7/8 |                                      :x:                                      |
|     Linux      |                             :heavy_check_mark:****                            |
|     macOS      |                                      :x:                                      |

\**** Please check the wiki for detailed compatibility information.

### Requirements

-   A supported standalone VR headset (see compatibility table above).
-   SteamVR.
-   A high-end gaming PC:
    -   See the OS compatibility table above.
    -   NVIDIA GPU with NVENC support (GTX 1000 series or newer), an AMD GPU with AMF VCE support, or an INTEL GPU with VPL support (Arc, Tiger Lake or newer), with the latest drivers.
    -   On laptops with both an integrated GPU (Intel HD, AMD iGPU) and a dedicated GPU (NVIDIA GTX/RTX, AMD HD/R5/R7), make sure to assign the dedicated GPU (or "high performance graphics adapter") to ALVR and SteamVR for the best performance and compatibility.  
        (NVIDIA: Nvidia Control Panel → 3D Settings → Application Settings; AMD: similar method)

-   Network:
    -   802.11ac 5 GHz Wi-Fi for the headset, and wired Ethernet for the PC is recommended.
    -   The PC and the headset must be connected to the same router (or use a routed connection as described [here](https://github.com/alvr-org/ALVR/wiki/ALVR-v14-and-Above)).

## Capture: screenshot and recording

PC-side capture runs in the **SteamVR driver process**. SteamVR + ALVR must be running. Global hotkeys work even when Dashboard is not focused.

### Screenshot (SBS JPEG)

- Default hotkey: **F8** (configurable).
- Dashboard: **Debug** tab → **Capture frame**.
- Output: side-by-side stereo RGB JPEG (after color correction, before FFR/YUV).
- Default folder: `Captures/Captures/` under the streamer root (same folder as `ALVR Dashboard.exe`).
- Filename: `screenshot_YYYYMMDD_HHMMSS_FOV_{horizontalDegrees}.jpg` (FOV is the connected headset's horizontal FOV in degrees).
- A short shutter sound plays if feedback sounds are enabled.

### Video recording (MKV)

- Default hotkey: **F9** — same key **toggles start / stop**.
- Dashboard: **Debug** tab → **Start recording** / **Stop recording**.
- Optional: **Extra → Capture → Start video recording at client connection**.
- On Windows, video is a **second encode of the pre-FFR SBS** (same layer as screenshots: real left/right, no packed midline swap). Headset streaming stays foveated. Disk encode uses a high VBR (~150 Mbps), not the Wi-Fi bitrate.
- If that encoder cannot start, recording falls back to copying the streamed bitstream (packed FFR).
- Game audio (loopback, not microphone) is muxed; ffmpeg remuxes a lossless MKV when you stop.
- Default folder: `Captures/Records/`.
- Filename: `recording_YYYYMMDD_HHMMSS_FOV_{horizontalDegrees}.mkv`.
- Distinct start / stop beeps if feedback sounds are enabled.
- Stop does not block SteamVR; ffmpeg remux finishes in the background.

**ffmpeg is required** for MKV recording. Put `ffmpeg` on `PATH`, or use `deps/windows/ffmpeg/bin/ffmpeg.exe` (dev tree) / `bin/win64/ffmpeg.exe` next to the streamer.

### Settings (`Extra` → `Capture`)

| Setting | Default | Notes |
| --- | --- | --- |
| Enable global hotkeys | on | Master switch |
| Screenshot hotkey | `F8` | `Ctrl` / `Alt` / `Shift` / `Win` + key, e.g. `Ctrl+F8`, `PrintScreen` |
| Recording hotkey | `F9` | Toggle start/stop |
| Feedback sounds | on | Screenshot / rec start / rec stop |
| Recording max FPS | 30 | `0` = same as stream. Caps disk encode only (headset unchanged). |
| Recording folder | `Captures/Records` | Relative to streamer root, or absolute |
| Screenshot folder | `Captures/Captures` | Relative to streamer root, or absolute |
| Rolling video files | off | Split files by duration (debug / bug reports) |

Do not bind screenshot and recording to the same key. Screenshot folder changes need a SteamVR restart.

### Older dual-file captures

Older builds wrote `recording.*.h264|h265|av1` plus a sidecar `.wav`. Current builds write MKV directly.

To remux leftover pairs, copy `mux-recordings.bat` to the streamer root (or keep it at the repo root) and double-click it. It looks for `Captures/Records`, muxes matching video + wav with stream copy, and skips files that already have an `.mkv`.

## Installation

Follow the [installation guide](https://github.com/alvr-org/ALVR/wiki/Installation-guide).

## Troubleshooting

-   See the [Troubleshooting](https://github.com/alvr-org/ALVR/wiki/Troubleshooting) page, and [Linux Troubleshooting](https://github.com/alvr-org/ALVR/wiki/Linux-Troubleshooting) if applicable.
-   Configuration recommendations and additional information can be found [here](https://github.com/alvr-org/ALVR/wiki/Information-and-Recommendations).

## Uninstallation

Open `ALVR Dashboard.exe`, go to the `Installation` tab, then press `Remove firewall rules`.  
Close the ALVR window and delete the ALVR folder.

## Build from Source

Follow the [build guide](https://github.com/alvr-org/ALVR/wiki/Building-From-Source).

## License

ALVR is licensed under the [MIT License](LICENSE).

## Privacy Policy

ALVR apps do not directly collect any personal data.

## Donate

If you would like to support this project, you can donate through our [Open Source Collective account](https://opencollective.com/alvr).

[badge-discord]: https://img.shields.io/discord/720612397580025886?style=for-the-badge&logo=discord&color=5865F2 "Join us on Discord"
[link-discord]: https://discord.gg/ALVR
[badge-matrix]: https://img.shields.io/static/v1?label=chat&message=%23alvr&style=for-the-badge&logo=matrix&color=blueviolet "Join us on Matrix"
[link-matrix]: https://matrix.to/#/#alvr:ckie.dev?via=ckie.dev
[badge-opencollective]: https://img.shields.io/opencollective/all/alvr?style=for-the-badge&logo=opencollective&color=79a3e6 "Donate"
[link-opencollective]: https://opencollective.com/alvr
