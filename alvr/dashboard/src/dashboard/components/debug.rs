use crate::dashboard::ServerRequest;
use eframe::egui::Ui;

pub fn debug_tab_ui(ui: &mut Ui) -> Option<ServerRequest> {
    let mut request = None;

    ui.label(
        "Capture frame saves an SBS JPEG into the configured screenshot folder (default Captures/Captures).
Start/Stop recording captures pre-FFR SBS (second encoder, high bitrate) + game audio into a lossless MKV. Headset stream stays FFR. Extra → Capture → Recording max FPS caps the file (0 = stream rate).
Global hotkeys (default F8 screenshot / F9 toggle recording) work while the ALVR SteamVR driver is running.
Requires ffmpeg on PATH or deps/windows/ffmpeg/bin.",
    );

    ui.columns(4, |ui| {
        if ui[0].button("Capture frame").clicked() {
            request = Some(ServerRequest::CaptureFrame);
        }

        if ui[1].button("Insert IDR").clicked() {
            request = Some(ServerRequest::InsertIdr);
        }

        if ui[2].button("Start recording").clicked() {
            request = Some(ServerRequest::StartRecording);
        }

        if ui[3].button("Stop recording").clicked() {
            request = Some(ServerRequest::StopRecording);
        }
    });

    request
}
