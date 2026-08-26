#pragma once
#include "shared/d3drender.h"

#include "shared/threadtools.h"

#include "FrameRender.h"
#include "VideoEncoder.h"
#include "VideoEncoderAMF.h"
#include "VideoEncoderNVENC.h"
#include "VideoEncoderVPL.h"
#include "alvr_server/Utils.h"
#include "d3d-render-utils/RenderPipeline.h"
#include <atomic>
#include <chrono>
#include <memory>
#include <mutex>
#include <thread>
#include <vector>
#include <d3d11.h>
#include <d3d11_1.h>
#include <map>
#include <wincodec.h>
#include <wincodecsdk.h>
#include <wrl.h>
#ifdef ALVR_GPL
#include "VideoEncoderSW.h"
#endif
#include "alvr_server/IDRScheduler.h"

using Microsoft::WRL::ComPtr;

//----------------------------------------------------------------------------
// Blocks on reading backbuffer from gpu, so WaitForPresent can return
// as soon as we know rendering made it this frame.  This step of the pipeline
// should run about 3ms per frame.
//----------------------------------------------------------------------------
class CEncoder : public CThread {
public:
    CEncoder();
    ~CEncoder();

    void Initialize(std::shared_ptr<CD3DRender> d3dRender);

    void SetViewParams(
        vr::HmdRect2_t projLeft,
        vr::HmdMatrix34_t eyeToHeadLeft,
        vr::HmdRect2_t projRight,
        vr::HmdMatrix34_t eyeToHeadRight
    );

    bool CopyToStaging(
        ID3D11Texture2D* pTexture[][2],
        vr::VRTextureBounds_t bounds[][2],
        vr::HmdMatrix34_t poses[],
        int layerCount,
        bool recentering,
        uint64_t presentationTime,
        uint64_t targetTimestampNs,
        const std::string& message,
        const std::string& debugText
    );

    virtual void Run();

    virtual void Stop();

    void NewFrameReady();

    void WaitForEncode();

    void OnStreamStart();

    void InsertIDR();

    void CaptureFrame();

    void StartRecordingEncode();
    void StopRecordingEncode();

private:
    void SaveScreenshotPng(ID3D11Texture2D* texture);
    void SyncRecordingEncoder();
    void EnsureRecordingWorker();
    void StopRecordingWorker();
    void RecordingWorker();
    void WaitRecordingIdle();

    CThreadEvent m_newFrameReady, m_encodeFinished;
    std::shared_ptr<VideoEncoder> m_videoEncoder;
    bool m_bExiting;
    uint64_t m_presentationTime;
    uint64_t m_targetTimestampNs;

    std::shared_ptr<FrameRender> m_FrameRender;
    std::shared_ptr<CD3DRender> m_d3dRender;
    std::atomic_bool m_captureFrame { false };
    std::atomic_bool m_wantRecordingEncode { false };
    std::shared_ptr<VideoEncoderNVENC> m_recordingEncoder;
    bool m_recordingNeedIdr = true;
    bool m_hasRecordingKeep = false;
    std::chrono::steady_clock::time_point m_lastRecordingKeep {};
    ComPtr<ID3D11Texture2D> m_recordingScaled;
    static const int kRecordingMaxSlots = 8;
    ComPtr<ID3D11Texture2D> m_recordingSlots[kRecordingMaxSlots];
    bool m_recordingSlotIdr[kRecordingMaxSlots] {};
    int m_recordingSlotCount = 3;
    std::mutex m_recordingQueueMutex;
    std::vector<int> m_recordingFree;
    std::vector<int> m_recordingJobs;
    std::atomic<unsigned> m_recordingInFlight { 0 };
    std::unique_ptr<d3d_render_utils::RenderPipeline> m_recordingBlit;
    std::mutex m_d3dCtxMutex;
    std::thread m_recordingThread;
    CThreadEvent m_recordingJobReady;
    std::atomic_bool m_recordingExit { false };

    IDRScheduler m_scheduler;
};
