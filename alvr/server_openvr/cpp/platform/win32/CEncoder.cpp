#include "CEncoder.h"

#include "alvr_server/Logger.h"
#include "alvr_server/Settings.h"

#include <chrono>
#include <filesystem>
#include <iomanip>
#include <objbase.h>
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
    m_newFrameReady.Set();
    Join();
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

void CEncoder::SaveScreenshotPng(ID3D11Texture2D* texture) {
    if (!texture || !m_d3dRender) {
        return;
    }

    D3D11_TEXTURE2D_DESC desc {};
    texture->GetDesc(&desc);

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

    ComPtr<ID3D11Texture2D> staging;
    HRESULT hr = m_d3dRender->GetDevice()->CreateTexture2D(&stagingDesc, nullptr, &staging);
    if (FAILED(hr)) {
        Error("Screenshot: CreateTexture2D staging failed %p\n", hr);
        return;
    }

    m_d3dRender->GetContext()->CopyResource(staging.Get(), texture);

    D3D11_MAPPED_SUBRESOURCE mapped {};
    hr = m_d3dRender->GetContext()->Map(staging.Get(), 0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        Error("Screenshot: Map failed %p\n", hr);
        return;
    }

    const UINT width = desc.Width;
    const UINT height = desc.Height;
    const UINT rowPitch = mapped.RowPitch;
    std::vector<uint8_t> pixels(static_cast<size_t>(rowPitch) * height);
    memcpy(pixels.data(), mapped.pData, pixels.size());
    m_d3dRender->GetContext()->Unmap(staging.Get(), 0);

    const bool isBgra = desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM
        || desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM_SRGB;

    std::string dir = Settings_Instance()->m_captureFrameDir;
    if (dir.empty()) {
        dir = ".";
    }
    std::error_code ec;
    std::filesystem::create_directories(dir, ec);

    auto now = std::chrono::system_clock::now();
    auto t = std::chrono::system_clock::to_time_t(now);
    std::tm tm {};
#if defined(_WIN32)
    localtime_s(&tm, &t);
#else
    localtime_r(&t, &tm);
#endif
    std::ostringstream name;
    name << dir << "/screenshot." << std::put_time(&tm, "%Y-%m-%d.%H-%M-%S") << ".png";
    std::wstring wpath(name.str().begin(), name.str().end());

    std::thread([pixels = std::move(pixels),
                 width,
                 height,
                 rowPitch,
                 isBgra,
                 wpath = std::move(wpath),
                 pathUtf8 = name.str()]() {
        HRESULT coHr = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        const bool shouldUninit = SUCCEEDED(coHr);

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
            Error("Screenshot: WIC factory failed %p\n", hr);
            cleanup();
            return;
        }

        ComPtr<IWICStream> stream;
        hr = factory->CreateStream(&stream);
        if (FAILED(hr) || FAILED(stream->InitializeFromFilename(wpath.c_str(), GENERIC_WRITE))) {
            Error("Screenshot: failed to open file %s\n", pathUtf8.c_str());
            cleanup();
            return;
        }

        ComPtr<IWICBitmapEncoder> encoder;
        hr = factory->CreateEncoder(GUID_ContainerFormatPng, nullptr, &encoder);
        if (FAILED(hr) || FAILED(encoder->Initialize(stream.Get(), WICBitmapEncoderNoCache))) {
            Error("Screenshot: PNG encoder init failed\n");
            cleanup();
            return;
        }

        ComPtr<IWICBitmapFrameEncode> frame;
        ComPtr<IPropertyBag2> props;
        hr = encoder->CreateNewFrame(&frame, &props);
        if (FAILED(hr) || FAILED(frame->Initialize(props.Get()))) {
            Error("Screenshot: frame init failed\n");
            cleanup();
            return;
        }

        frame->SetSize(width, height);
        WICPixelFormatGUID format = GUID_WICPixelFormat32bppRGBA;
        frame->SetPixelFormat(&format);

        // Convert BGRA -> RGBA if needed into tightly packed buffer
        std::vector<uint8_t> packed(static_cast<size_t>(width) * height * 4);
        for (UINT y = 0; y < height; ++y) {
            const uint8_t* src = pixels.data() + static_cast<size_t>(y) * rowPitch;
            uint8_t* dst = packed.data() + static_cast<size_t>(y) * width * 4;
            for (UINT x = 0; x < width; ++x) {
                if (isBgra) {
                    dst[x * 4 + 0] = src[x * 4 + 2];
                    dst[x * 4 + 1] = src[x * 4 + 1];
                    dst[x * 4 + 2] = src[x * 4 + 0];
                    dst[x * 4 + 3] = src[x * 4 + 3];
                } else {
                    dst[x * 4 + 0] = src[x * 4 + 0];
                    dst[x * 4 + 1] = src[x * 4 + 1];
                    dst[x * 4 + 2] = src[x * 4 + 2];
                    dst[x * 4 + 3] = src[x * 4 + 3];
                }
            }
        }

        hr = frame->WritePixels(height, width * 4, static_cast<UINT>(packed.size()), packed.data());
        if (FAILED(hr) || FAILED(frame->Commit()) || FAILED(encoder->Commit())) {
            Error("Screenshot: write PNG failed for %s\n", pathUtf8.c_str());
        } else {
            Info("Screenshot saved: %s\n", pathUtf8.c_str());
        }

        cleanup();
    }).detach();
}
