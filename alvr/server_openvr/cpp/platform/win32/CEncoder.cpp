#include "CEncoder.h"

#include "ALVR-common/packet_types.h"
#include "alvr_server/Logger.h"
#include "alvr_server/Settings.h"
#include "alvr_server/Utils.h"
#include "alvr_server/bindings.h"
#include "d3d-render-utils/RenderUtils.h"

#include <algorithm>
#include <cmath>

#include <atomic>
#include <chrono>
#include <filesystem>
#include <iomanip>
#include <objbase.h>
#include <oleauto.h>
#include <sstream>
#include <thread>
#include <vector>
#include <wincodec.h>

CEncoder::CEncoder()
    : m_bExiting(false)
    , m_targetTimestampNs(0) {
    m_encodeFinished.Set();
}

CEncoder::~CEncoder() {
    if (m_recordingEncoder) {
        m_recordingEncoder->Shutdown();
        m_recordingEncoder.reset();
    }
    m_recordingBlit.reset();
    m_recordingScaled.Reset();
    if (m_videoEncoder) {
        m_videoEncoder->Shutdown();
        m_videoEncoder.reset();
    }
}

void CEncoder::Initialize(std::shared_ptr<CD3DRender> d3dRender) {
    m_d3dRender = d3dRender;
    m_FrameRender = std::make_shared<FrameRender>(d3dRender);
    m_FrameRender->Startup();
    uint32_t encoderWidth, encoderHeight;
    m_FrameRender->GetEncodingResolution(&encoderWidth, &encoderHeight);

    Exception vplException;
    Exception vceException;
    Exception nvencException;
#ifdef ALVR_GPL
    Exception swException;

    if (Settings_Instance()->m_forceSwEncoding) {
        try {
            Debug("Try to use VideoEncoderSW.\n");
            m_videoEncoder
                = std::make_shared<VideoEncoderSW>(d3dRender, encoderWidth, encoderHeight);
            m_videoEncoder->Initialize();
            return;
        } catch (Exception e) {
            swException = e;
        }
    }
#endif

    try {
        Debug("Try to use VideoEncoderAMF.\n");
        m_videoEncoder = std::make_shared<VideoEncoderAMF>(d3dRender, encoderWidth, encoderHeight);
        m_videoEncoder->Initialize();
        return;
    } catch (Exception e) {
        vceException = e;
    }
    try {
        Debug("Try to use VideoEncoderNVENC.\n");
        m_videoEncoder
            = std::make_shared<VideoEncoderNVENC>(d3dRender, encoderWidth, encoderHeight);
        m_videoEncoder->Initialize();
        return;
    } catch (Exception e) {
        nvencException = e;
    }
    try {
        Debug("Try to use VideoEncoderVPL.\n");
        m_videoEncoder = std::make_shared<VideoEncoderVPL>(d3dRender, encoderWidth, encoderHeight);
        m_videoEncoder->Initialize();
        return;
    } catch (Exception e) {
        vplException = e;
    }
#ifdef ALVR_GPL
    try {
        Debug("Try to use VideoEncoderSW.\n");
        m_videoEncoder = std::make_shared<VideoEncoderSW>(d3dRender, encoderWidth, encoderHeight);
        m_videoEncoder->Initialize();
        return;
    } catch (Exception e) {
        swException = e;
    }
    throw MakeException(
        "All VideoEncoder are not available. VCE: %s, NVENC: %s, VPL: %s, SW: %s",
        vceException.what(),
        nvencException.what(),
        vplException.what(),
        swException.what()
    );
#else
    throw MakeException(
        "All VideoEncoder are not available. VCE: %s, NVENC: %s, VPL: %s",
        vceException.what(),
        nvencException.what(),
        vplException.what()
    );
#endif
}

void CEncoder::SetViewParams(
    vr::HmdRect2_t projLeft,
    vr::HmdMatrix34_t eyeToHeadLeft,
    vr::HmdRect2_t projRight,
    vr::HmdMatrix34_t eyeToHeadRight
) {
    m_FrameRender->SetViewParams(projLeft, eyeToHeadLeft, projRight, eyeToHeadRight);
}

bool CEncoder::CopyToStaging(
    ID3D11Texture2D* pTexture[][2],
    vr::VRTextureBounds_t bounds[][2],
    vr::HmdMatrix34_t poses[],
    int layerCount,
    bool recentering,
    uint64_t presentationTime,
    uint64_t targetTimestampNs,
    const std::string& message,
    const std::string& debugText
) {
    m_presentationTime = presentationTime;
    m_targetTimestampNs = targetTimestampNs;
    m_FrameRender->Startup();

    m_FrameRender->RenderFrame(
        pTexture, bounds, poses, layerCount, recentering, message, debugText
    );
    return true;
}

void CEncoder::Run() {
    Debug("CEncoder: Start thread. Id=%d\n", GetCurrentThreadId());
    SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_MOST_URGENT);

    while (!m_bExiting) {
        m_newFrameReady.Wait();
        if (m_bExiting)
            break;

        if (m_FrameRender->GetTexture()) {
            if (m_captureFrame.exchange(false)) {
                if (auto shot = m_FrameRender->GetScreenshotTexture()) {
                    SaveScreenshotPng(shot.Get());
                } else {
                    Error("Screenshot requested but no RGB texture available\n");
                }
            }

            SyncRecordingEncoder();
            if (m_recordingEncoder) {
                if (auto shot = m_FrameRender->GetScreenshotTexture()) {
                    const float maxFps = GetRecordingMaxFps();
                    const auto now = std::chrono::steady_clock::now();
                    bool drop = false;
                    if (maxFps >= 1.0f && m_hasRecordingKeep && !m_recordingNeedIdr) {
                        const auto minInterval = std::chrono::duration<double>(1.0 / (double)maxFps);
                        if ((now - m_lastRecordingKeep) < minInterval) {
                            drop = true;
                        }
                    }
                    if (!drop) {
                        ID3D11Texture2D* src = shot.Get();
                        if (m_recordingBlit && m_recordingScaled) {
                            m_recordingBlit->Render();
                            src = m_recordingScaled.Get();
                        }
                        bool idr = m_recordingNeedIdr;
                        m_recordingNeedIdr = false;
                        m_lastRecordingKeep = now;
                        m_hasRecordingKeep = true;
                        m_recordingEncoder->Transmit(
                            src, m_presentationTime, m_targetTimestampNs, idr
                        );
                    }
                }
            }

            m_videoEncoder->Transmit(
                m_FrameRender->GetTexture().Get(),
                m_presentationTime,
                m_targetTimestampNs,
                m_scheduler.CheckIDRInsertion()
            );
        }

        m_encodeFinished.Set();
    }
}

void CEncoder::Stop() {
    m_bExiting = true;
    m_wantRecordingEncode.store(false);
    m_newFrameReady.Set();
    Join();
    if (m_recordingEncoder) {
        m_recordingEncoder->Shutdown();
        m_recordingEncoder.reset();
    }
    m_recordingBlit.reset();
    m_recordingScaled.Reset();
    m_FrameRender.reset();
}

void CEncoder::NewFrameReady() {
    m_encodeFinished.Reset();
    m_newFrameReady.Set();
}

void CEncoder::WaitForEncode() { m_encodeFinished.Wait(); }

void CEncoder::OnStreamStart() { m_scheduler.OnStreamStart(); }

void CEncoder::InsertIDR() { m_scheduler.InsertIDR(); }

void CEncoder::CaptureFrame() { m_captureFrame = true; }

void CEncoder::StartRecordingEncode() {
    m_recordingNeedIdr = true;
    m_hasRecordingKeep = false;
    m_wantRecordingEncode.store(true);
}

void CEncoder::StopRecordingEncode() { m_wantRecordingEncode.store(false); }

static int Align32(int v) {
    v &= ~31;
    return v < 32 ? 32 : v;
}

static void FitNvencSize(int srcW, int srcH, int maxDim, int* outW, int* outH) {
    float scale = 1.0f;
    if (srcW > maxDim) {
        scale = (float)maxDim / (float)srcW;
    }
    if ((int)(srcH * scale) > maxDim) {
        scale = (float)maxDim / (float)srcH;
    }
    *outW = Align32((int)std::lround((float)srcW * scale));
    *outH = Align32((int)std::lround((float)srcH * scale));
}

void CEncoder::SyncRecordingEncoder() {
    const bool want = m_wantRecordingEncode.load();
    if (want && !m_recordingEncoder) {
        auto shot = m_FrameRender ? m_FrameRender->GetScreenshotTexture() : nullptr;
        int srcW = Settings_Instance()->m_renderWidth;
        int srcH = Settings_Instance()->m_renderHeight;
        if (shot) {
            D3D11_TEXTURE2D_DESC desc {};
            shot->GetDesc(&desc);
            srcW = (int)desc.Width;
            srcH = (int)desc.Height;
        }

        const int streamCodec = Settings_Instance()->m_codec;
        Exception lastErr("no encoder attempted");
        bool started = false;

        auto tryStart = [&](int codec, int maxDim) {
            int encW = 0, encH = 0;
            FitNvencSize(srcW, srcH, maxDim, &encW, &encH);
            Info(
                "Starting pre-FFR recording encoder %dx%d (src %dx%d) codec=%d\n",
                encW,
                encH,
                srcW,
                srcH,
                codec
            );
            auto enc = std::make_shared<VideoEncoderNVENC>(
                m_d3dRender, encW, encH, true, codec
            );
            enc->Initialize();

            m_recordingBlit.reset();
            m_recordingScaled.Reset();
            if (encW != srcW || encH != srcH) {
                if (!shot) {
                    throw MakeException("No screenshot texture to scale for recording");
                }
                DXGI_FORMAT fmt = DXGI_FORMAT_R8G8B8A8_UNORM_SRGB;
                D3D11_TEXTURE2D_DESC srcDesc {};
                shot->GetDesc(&srcDesc);
                fmt = srcDesc.Format;
                m_recordingScaled.Attach(d3d_render_utils::CreateTexture(
                    m_d3dRender->GetDevice(), (uint32_t)encW, (uint32_t)encH, fmt
                ));
                std::vector<uint8_t> quadCso(
                    QUAD_SHADER_CSO_PTR, QUAD_SHADER_CSO_PTR + QUAD_SHADER_CSO_LEN
                );
                std::vector<uint8_t> blitCso(
                    COLOR_CORRECTION_CSO_PTR,
                    COLOR_CORRECTION_CSO_PTR + COLOR_CORRECTION_CSO_LEN
                );
                struct ColorCorrection {
                    float renderWidth;
                    float renderHeight;
                    float brightness;
                    float contrast;
                    float saturation;
                    float gamma;
                    float sharpening;
                    float _align;
                };
                ColorCorrection cc
                    = { (float)srcW, (float)srcH, 0.f, 1.f, 1.f, 1.f, 0.f, 0.f };
                auto* buf = d3d_render_utils::CreateBuffer(m_d3dRender->GetDevice(), cc);
                auto pipeline = std::make_unique<d3d_render_utils::RenderPipeline>(
                    m_d3dRender->GetDevice()
                );
                ComPtr<ID3D11VertexShader> vs
                    = d3d_render_utils::CreateVertexShader(m_d3dRender->GetDevice(), quadCso);
                pipeline->Initialize(
                    { shot.Get() }, vs.Get(), blitCso, m_recordingScaled.Get(), buf
                );
                m_recordingBlit = std::move(pipeline);
            }

            m_recordingEncoder = enc;
            m_recordingNeedIdr = true;
            SetRecordingMuxStreamFallback(false);
        };

        const int attemptCodec[] = { streamCodec, ALVR_CODEC_H264 };
        const int attemptMax[]
            = { streamCodec == ALVR_CODEC_H264 ? 4096 : 8192, 4096 };
        for (int i = 0; i < 2; ++i) {
            if (i > 0 && attemptCodec[i] == streamCodec) {
                continue;
            }
            try {
                tryStart(attemptCodec[i], attemptMax[i]);
                started = true;
                break;
            } catch (Exception e) {
                lastErr = e;
                Warn(
                    "Pre-FFR recording encoder codec=%d failed: %s\n",
                    attemptCodec[i],
                    e.what()
                );
                m_recordingEncoder.reset();
                m_recordingBlit.reset();
                m_recordingScaled.Reset();
            }
        }
        if (!started) {
            Warn(
                "Pre-FFR recording encoder unavailable (%s); muxing streamed bitstream\n",
                lastErr.what()
            );
            m_wantRecordingEncode.store(false);
        }
    } else if (!want && m_recordingEncoder) {
        Info("Stopping pre-FFR recording encoder\n");
        m_recordingEncoder->Shutdown();
        m_recordingEncoder.reset();
        m_recordingBlit.reset();
        m_recordingScaled.Reset();
        SetRecordingMuxStreamFallback(false);
    }
}

static std::wstring Utf8ToWide(const std::string& s) {
    if (s.empty()) {
        return std::wstring();
    }
    int n = MultiByteToWideChar(CP_UTF8, 0, s.c_str(), (int)s.size(), nullptr, 0);
    if (n <= 0) {
        return std::wstring(s.begin(), s.end());
    }
    std::wstring out(n, L'\0');
    MultiByteToWideChar(CP_UTF8, 0, s.c_str(), (int)s.size(), out.data(), n);
    return out;
}

void CEncoder::SaveScreenshotPng(ID3D11Texture2D* texture) {
    // Must never throw out of the encoder thread — that takes down SteamVR.
    try {
        if (!texture || !m_d3dRender) {
            return;
        }

        // Avoid overlapping captures (re-entry / double F8).
        static std::atomic_bool s_busy { false };
        bool expected = false;
        if (!s_busy.compare_exchange_strong(expected, true)) {
            Info("Screenshot: already in progress, ignoring\n");
            return;
        }
        // RAII clear of busy flag if we return early before spawning worker
        struct BusyGuard {
            std::atomic_bool* flag;
            bool release_early;
            ~BusyGuard() {
                if (release_early && flag) {
                    flag->store(false);
                }
            }
        } busy_guard { &s_busy, true };

        D3D11_TEXTURE2D_DESC desc {};
        texture->GetDesc(&desc);

        if (desc.Width == 0 || desc.Height == 0) {
            Error("Screenshot: invalid texture size\n");
            return;
        }

        // Only 8-bit RGBA/BGRA family for WIC path
        if (desc.Format != DXGI_FORMAT_R8G8B8A8_UNORM
            && desc.Format != DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
            && desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
            && desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM_SRGB) {
            Error("Screenshot: unsupported texture format %u\n", (unsigned)desc.Format);
            return;
        }

        D3D11_TEXTURE2D_DESC stagingDesc = desc;
        stagingDesc.Usage = D3D11_USAGE_STAGING;
        stagingDesc.BindFlags = 0;
        stagingDesc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        stagingDesc.MiscFlags = 0;
        stagingDesc.MipLevels = 1;
        stagingDesc.ArraySize = 1;
        stagingDesc.SampleDesc.Count = 1;
        stagingDesc.SampleDesc.Quality = 0;

        ComPtr<ID3D11Texture2D> staging;
        HRESULT hr = m_d3dRender->GetDevice()->CreateTexture2D(&stagingDesc, nullptr, &staging);
        if (FAILED(hr) || !staging) {
            Error("Screenshot: CreateTexture2D staging failed 0x%08lx\n", hr);
            return;
        }

        auto* ctx = m_d3dRender->GetContext();
        ctx->CopyResource(staging.Get(), texture);
        // Ensure GPU finished copy before Map (avoids device-removal style failures).
        ctx->Flush();

        D3D11_MAPPED_SUBRESOURCE mapped {};
        hr = ctx->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped);
        if (FAILED(hr) || !mapped.pData) {
            Error("Screenshot: Map failed 0x%08lx\n", hr);
            return;
        }

        const UINT width = desc.Width;
        const UINT height = desc.Height;
        const UINT rowPitch = mapped.RowPitch;
        const size_t nbytes = static_cast<size_t>(rowPitch) * static_cast<size_t>(height);
        std::vector<uint8_t> pixels(nbytes);
        memcpy(pixels.data(), mapped.pData, nbytes);
        ctx->Unmap(staging.Get(), 0);
        staging.Reset();

        const bool isBgra = desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM
            || desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM_SRGB;

        std::string dir = Settings_Instance()->m_captureFrameDir;
        if (dir.empty()) {
            dir = ".";
        }
        std::error_code ec;
        std::filesystem::create_directories(dir, ec);
        if (ec) {
            Error("Screenshot: create_directories failed for %s\n", dir.c_str());
            return;
        }

        const std::string pathUtf8
            = FormatCaptureFilePath(dir, "screenshot", "jpg", GetHeadsetHFovDeg());
        const std::wstring wpath = Utf8ToWide(pathUtf8);

        // JPEG encode off the encoder thread so we don't stall the VR frame loop.
        busy_guard.release_early = false;
        std::thread([pixels = std::move(pixels),
                     width,
                     height,
                     rowPitch,
                     isBgra,
                     wpath,
                     pathUtf8,
                     busy = &s_busy]() {
            struct ClearBusy {
                std::atomic_bool* f;
                ~ClearBusy() { f->store(false); }
            } clear { busy };

            try {
                HRESULT coHr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
                const bool shouldUninit = (coHr == S_OK || coHr == S_FALSE);

                auto cleanup = [&]() {
                    if (shouldUninit) {
                        CoUninitialize();
                    }
                };

                ComPtr<IWICImagingFactory> factory;
                HRESULT hr = CoCreateInstance(
                    CLSID_WICImagingFactory,
                    nullptr,
                    CLSCTX_INPROC_SERVER,
                    IID_PPV_ARGS(&factory)
                );
                if (FAILED(hr)) {
                    Error("Screenshot: WIC factory failed 0x%08lx\n", hr);
                    cleanup();
                    return;
                }

                ComPtr<IWICStream> stream;
                hr = factory->CreateStream(&stream);
                if (FAILED(hr)
                    || FAILED(stream->InitializeFromFilename(wpath.c_str(), GENERIC_WRITE))) {
                    Error("Screenshot: failed to open file %s\n", pathUtf8.c_str());
                    cleanup();
                    return;
                }

                ComPtr<IWICBitmapEncoder> encoder;
                hr = factory->CreateEncoder(GUID_ContainerFormatJpeg, nullptr, &encoder);
                if (FAILED(hr)
                    || FAILED(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache))) {
                    Error("Screenshot: JPEG encoder init failed\n");
                    cleanup();
                    return;
                }

                ComPtr<IWICBitmapFrameEncode> frame;
                ComPtr<IPropertyBag2> props;
                hr = encoder->CreateNewFrame(&frame, &props);
                if (FAILED(hr)) {
                    Error("Screenshot: frame create failed\n");
                    cleanup();
                    return;
                }
                if (props) {
                    PROPBAG2 opt {};
                    opt.pstrName = const_cast<LPOLESTR>(L"ImageQuality");
                    VARIANT val;
                    VariantInit(&val);
                    val.vt = VT_R4;
                    val.fltVal = 0.92f;
                    props->Write(1, &opt, &val);
                    VariantClear(&val);
                }
                if (FAILED(frame->Initialize(props.Get()))) {
                    Error("Screenshot: frame init failed\n");
                    cleanup();
                    return;
                }

                frame->SetSize(width, height);
                WICPixelFormatGUID format = GUID_WICPixelFormat24bppBGR;
                frame->SetPixelFormat(&format);

                std::vector<uint8_t> packed(static_cast<size_t>(width) * height * 3);
                for (UINT y = 0; y < height; ++y) {
                    const uint8_t* src = pixels.data() + static_cast<size_t>(y) * rowPitch;
                    uint8_t* dst = packed.data() + static_cast<size_t>(y) * width * 3;
                    for (UINT x = 0; x < width; ++x) {
                        const uint8_t r = isBgra ? src[x * 4 + 2] : src[x * 4 + 0];
                        const uint8_t g = src[x * 4 + 1];
                        const uint8_t b = isBgra ? src[x * 4 + 0] : src[x * 4 + 2];
                        dst[x * 3 + 0] = b;
                        dst[x * 3 + 1] = g;
                        dst[x * 3 + 2] = r;
                    }
                }

                const UINT stride = width * 3;
                hr = frame->WritePixels(
                    height, stride, static_cast<UINT>(packed.size()), packed.data()
                );
                if (FAILED(hr) || FAILED(frame->Commit()) || FAILED(encoder->Commit())) {
                    Error("Screenshot: write JPEG failed for %s\n", pathUtf8.c_str());
                } else {
                    Info("Screenshot saved: %s\n", pathUtf8.c_str());
                }

                cleanup();
            } catch (const std::exception& e) {
                Error("Screenshot worker exception: %s\n", e.what());
            } catch (...) {
                Error("Screenshot worker unknown exception\n");
            }
        }).detach();
    } catch (const std::exception& e) {
        Error("Screenshot exception: %s\n", e.what());
    } catch (...) {
        Error("Screenshot unknown exception\n");
    }
}
