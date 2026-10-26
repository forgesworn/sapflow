#ifndef SAPFLOW_FFI_H
#define SAPFLOW_FFI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    uint8_t *ptr;
    size_t len;
} SapflowBuffer;

#define SAPFLOW_ABI_VERSION 1

#define SAPFLOW_OBJECT_ENCODED_BUFFER 1
#define SAPFLOW_OBJECT_MEDIA_CODEC 2
#define SAPFLOW_OBJECT_MEDIA_SAMPLE 3
#define SAPFLOW_OBJECT_AAC_TIMING_VIEWS 4
#define SAPFLOW_OBJECT_SAMPLE_FANOUT 5
#define SAPFLOW_OBJECT_RECORDING_CHECKPOINT 6
#define SAPFLOW_OBJECT_DECODED_RECORDING_CHECKPOINT 7
#define SAPFLOW_OBJECT_PROGRAMME_CHECKPOINT 8
#define SAPFLOW_OBJECT_DECODED_PROGRAMME_CHECKPOINT 9
#define SAPFLOW_OBJECT_RECORDING_MUXER 10
#define SAPFLOW_OBJECT_RTMP_PUBLISHER 11
#define SAPFLOW_OBJECT_SESSION 12
#define SAPFLOW_OBJECT_SRT_PUBLISHER 13

#define SAPFLOW_OP_SDK_ABI_VERSION 1
#define SAPFLOW_OP_ENCODED_BUFFER_NEW 2
#define SAPFLOW_OP_ENCODED_BUFFER_LEN 3
#define SAPFLOW_OP_ENCODED_BUFFER_IS_EMPTY 4
#define SAPFLOW_OP_ENCODED_BUFFER_BYTE_AT 5
#define SAPFLOW_OP_MEDIA_CODEC_H264 6
#define SAPFLOW_OP_MEDIA_CODEC_AAC 7
#define SAPFLOW_OP_MEDIA_CODEC_SAMPLE 8
#define SAPFLOW_OP_MEDIA_SAMPLE_AAC_TIMING_VIEWS 9
#define SAPFLOW_OP_MEDIA_SAMPLE_AAC_SOURCE_INFO 10
#define SAPFLOW_OP_MEDIA_SAMPLE_LEN 11
#define SAPFLOW_OP_MEDIA_SAMPLE_IS_EMPTY 12
#define SAPFLOW_OP_MEDIA_SAMPLE_IS_VIDEO 13
#define SAPFLOW_OP_MEDIA_SAMPLE_IS_KEYFRAME 14
#define SAPFLOW_OP_MEDIA_SAMPLE_PTS 15
#define SAPFLOW_OP_MEDIA_SAMPLE_DTS 16
#define SAPFLOW_OP_MEDIA_SAMPLE_DURATION 17
#define SAPFLOW_OP_MEDIA_SAMPLE_TIMESCALE 18
#define SAPFLOW_OP_AAC_TIMING_VIEWS_SOURCE_INFO 19
#define SAPFLOW_OP_AAC_TIMING_VIEWS_RECORDING_SAMPLE 20
#define SAPFLOW_OP_AAC_TIMING_VIEWS_PUBLISHING_SAMPLE 21
#define SAPFLOW_OP_SAMPLE_FANOUT_NEW 22
#define SAPFLOW_OP_SAMPLE_FANOUT_WITH_AAC_TIMING_VIEWS 23
#define SAPFLOW_OP_SAMPLE_FANOUT_PUSH_AAC_TIMING_VIEWS 24
#define SAPFLOW_OP_SAMPLE_FANOUT_PUSH 25
#define SAPFLOW_OP_SAMPLE_FANOUT_POP_PUBLISHING 26
#define SAPFLOW_OP_SAMPLE_FANOUT_POP_RECORDING 27
#define SAPFLOW_OP_SAMPLE_FANOUT_STATS 28
#define SAPFLOW_OP_SAMPLE_FANOUT_CLOSE_PUBLISHING_ADMISSION 29
#define SAPFLOW_OP_SAMPLE_FANOUT_CLOSE_RECORDING_ADMISSION 30
#define SAPFLOW_OP_SAMPLE_FANOUT_CANCEL 31
#define SAPFLOW_OP_RECORDING_CHECKPOINT_FACTS 32
#define SAPFLOW_OP_RECORDING_CHECKPOINT_PRESENTATION_EXTENT 33
#define SAPFLOW_OP_DECODED_RECORDING_CHECKPOINT_INFO 34
#define SAPFLOW_OP_DECODED_RECORDING_CHECKPOINT_HEADER 35
#define SAPFLOW_OP_DECODED_RECORDING_CHECKPOINT_FRAGMENT_REGION_SHA256 36
#define SAPFLOW_OP_PROGRAMME_CHECKPOINT_FACTS 37
#define SAPFLOW_OP_PROGRAMME_CHECKPOINT_START 38
#define SAPFLOW_OP_PROGRAMME_CHECKPOINT_WINDOW_END 39
#define SAPFLOW_OP_PROGRAMME_CHECKPOINT_PRESENTATION_EXTENT 40
#define SAPFLOW_OP_DECODED_PROGRAMME_CHECKPOINT_INFO 41
#define SAPFLOW_OP_DECODED_PROGRAMME_CHECKPOINT_HEADER 42
#define SAPFLOW_OP_DECODED_PROGRAMME_CHECKPOINT_FRAGMENT_REGION_SHA256 43
#define SAPFLOW_OP_DECODED_PROGRAMME_CHECKPOINT_PRESENTATION_EXTENT 44
#define SAPFLOW_OP_RECORDING_MUXER_NEW 45
#define SAPFLOW_OP_RECORDING_MUXER_WITH_NATIVE_AAC_TIMING 46
#define SAPFLOW_OP_RECORDING_MUXER_WITH_PROGRAMME_AAC_TIMING 47
#define SAPFLOW_OP_RECORDING_MUXER_BEGIN_PROGRAMME_WINDOW 48
#define SAPFLOW_OP_RECORDING_MUXER_CLOSE_PROGRAMME_WINDOW 49
#define SAPFLOW_OP_RECORDING_MUXER_TAKE_PROGRAMME_OUTPUT 50
#define SAPFLOW_OP_RECORDING_MUXER_PROGRAMME_CHECKPOINT_HEADER 51
#define SAPFLOW_OP_RECORDING_MUXER_ENCODE_PROGRAMME_CHECKPOINT 52
#define SAPFLOW_OP_RECORDING_MUXER_FINISH_PROGRAMME 53
#define SAPFLOW_OP_RECORDING_MUXER_BEGIN_NATIVE_AUDIO 54
#define SAPFLOW_OP_RECORDING_MUXER_END_NATIVE_AUDIO_GENERATION 55
#define SAPFLOW_OP_RECORDING_MUXER_COMPLETE_NATIVE_AUDIO 56
#define SAPFLOW_OP_RECORDING_MUXER_PUSH 57
#define SAPFLOW_OP_RECORDING_MUXER_TAKE_OUTPUT 58
#define SAPFLOW_OP_RECORDING_MUXER_TAKE_NATIVE_OUTPUT 59
#define SAPFLOW_OP_RECORDING_MUXER_CHECKPOINT_HEADER 60
#define SAPFLOW_OP_RECORDING_MUXER_ENCODE_CHECKPOINT 61
#define SAPFLOW_OP_RECORDING_MUXER_FINISH 62
#define SAPFLOW_OP_RECORDING_MUXER_FINISH_NATIVE 63
#define SAPFLOW_OP_RECORDING_MUXER_CANCEL 64
#define SAPFLOW_OP_RECORDING_MUXER_STATS 65
#define SAPFLOW_OP_DECODE_RECORDING_CHECKPOINT 66
#define SAPFLOW_OP_VALIDATE_RECORDING_CHECKPOINT_SUCCESSOR 67
#define SAPFLOW_OP_DECODE_PROGRAMME_CHECKPOINT 68
#define SAPFLOW_OP_VALIDATE_PROGRAMME_CHECKPOINT_SUCCESSOR 69
#define SAPFLOW_OP_RTMP_PUBLISHER_NEW 70
#define SAPFLOW_OP_RTMP_PUBLISHER_DECLARE_TRACKS 71
#define SAPFLOW_OP_RTMP_PUBLISHER_POLL_EVENT 72
#define SAPFLOW_OP_RTMP_PUBLISHER_FEED 73
#define SAPFLOW_OP_RTMP_PUBLISHER_TICK 74
#define SAPFLOW_OP_RTMP_PUBLISHER_NEXT_DEADLINE_MS 75
#define SAPFLOW_OP_RTMP_PUBLISHER_TAKE_OUTPUT 76
#define SAPFLOW_OP_RTMP_PUBLISHER_QUEUED_OUTPUT_BYTES 77
#define SAPFLOW_OP_RTMP_PUBLISHER_STATE 78
#define SAPFLOW_OP_RTMP_PUBLISHER_SEND 79
#define SAPFLOW_OP_RTMP_PUBLISHER_SEND_AND_TAKE_OUTPUT 80
#define SAPFLOW_OP_RTMP_PUBLISHER_FINISH 81
#define SAPFLOW_OP_RTMP_PUBLISHER_CANCEL 82
#define SAPFLOW_OP_SESSION_NEW 83
#define SAPFLOW_OP_SESSION_START 84
#define SAPFLOW_OP_SESSION_PREPARED 85
#define SAPFLOW_OP_SESSION_CONNECTED 86
#define SAPFLOW_OP_SESSION_SENDING 87
#define SAPFLOW_OP_SESSION_TRANSPORT_RECOVERED 88
#define SAPFLOW_OP_SESSION_MEDIA_CONFIRMED 89
#define SAPFLOW_OP_SESSION_MEDIA_CONFIRMED_FOR_ATTEMPT 90
#define SAPFLOW_OP_SESSION_REPLACE_TRANSPORT 91
#define SAPFLOW_OP_SESSION_TRANSPORT_FAILED 92
#define SAPFLOW_OP_SESSION_TICK 93
#define SAPFLOW_OP_SESSION_INTERRUPT 94
#define SAPFLOW_OP_SESSION_STOP 95
#define SAPFLOW_OP_SESSION_FINISH_STOP 96
#define SAPFLOW_OP_SESSION_CANCEL 97
#define SAPFLOW_OP_SESSION_SNAPSHOT 98
#define SAPFLOW_OP_SRT_PUBLISHER_NEW 99
#define SAPFLOW_OP_SRT_PUBLISHER_PUSH 100
#define SAPFLOW_OP_SRT_PUBLISHER_FEED 101
#define SAPFLOW_OP_SRT_PUBLISHER_TICK 102
#define SAPFLOW_OP_SRT_PUBLISHER_POLL_DATAGRAM 103
#define SAPFLOW_OP_SRT_PUBLISHER_IS_READY 104
#define SAPFLOW_OP_SRT_PUBLISHER_HAS_IMMEDIATE_WORK 105
#define SAPFLOW_OP_SRT_PUBLISHER_NEXT_DEADLINE_US 106
#define SAPFLOW_OP_SRT_PUBLISHER_STATE 107
#define SAPFLOW_OP_SRT_PUBLISHER_POLL_EVENT 108
#define SAPFLOW_OP_SRT_PUBLISHER_STOP 109
#define SAPFLOW_OP_SRT_PUBLISHER_SOCKET_SEND_FAILED 110
#define SAPFLOW_OP_SRT_PUBLISHER_STATS 111

/* Requests and responses use the ABI's bounded little-endian value encoding.
 * Returned buffers are owned by the caller and freed exactly once. Object
 * handles are retained Arc owners and must be freed with their actual kind. */
uint32_t sapflow_abi_version(void);
int32_t sapflow_call(uint32_t operation, const void *handle,
    const uint8_t *input, size_t input_len,
    SapflowBuffer *out, SapflowBuffer *error);
void sapflow_buffer_free(SapflowBuffer buffer);
void sapflow_object_free(uint32_t kind, void *handle);

typedef struct {
    bool publishing_queued;
    bool recording_queued;
    bool publishing_waiting_keyframe;
    uint64_t publishing_overflows;
    bool recording_failed;
} SapflowFanoutOutcome;

typedef struct {
    void *handle;
    uint64_t len;
    int64_t pts;
    int64_t dts;
    uint32_t duration;
    uint32_t timescale;
    bool is_video;
    bool is_keyframe;
} SapflowMediaSampleResult;

typedef struct {
    uint64_t value;
    bool present;
} SapflowOptionalU64;

typedef struct {
    SapflowBuffer bytes;
    void *checkpoint;
    uint8_t kind;
    bool rewrite_header;
    bool present;
} SapflowRecordingOutput;

typedef struct {
    SapflowBuffer output;
    uint64_t queued_output_bytes;
    uint64_t next_deadline_ms;
    uint8_t state;
    bool has_next_deadline;
    bool startup_submitted;
    bool peer_media_progress;
} SapflowRtmpSendOutput;

typedef struct {
    SapflowBuffer destination;
    SapflowBuffer bytes;
    bool present;
} SapflowSrtDatagram;

/* Hot calls borrow input only for the synchronous call. Rust performs the
 * existing single compressed ownership copy. No serialised payload is made.
 * Result buffers and handles are caller-owned, including on adapter failures. */
int32_t sapflow_encoded_buffer_new(const uint8_t *bytes, size_t len,
    void **out, SapflowBuffer *error);
int32_t sapflow_encoded_buffer_scalar(const void *buffer, uint32_t operation,
    uint64_t index, uint64_t *out, SapflowBuffer *error);
int32_t sapflow_media_codec_sample(const void *codec,
    const uint8_t *bytes, size_t len, int64_t pts, int64_t dts,
    uint32_t duration, uint32_t timescale, bool keyframe,
    void **out, SapflowBuffer *error);
int32_t sapflow_media_sample_scalar(const void *sample, uint32_t operation,
    uint64_t *out, SapflowBuffer *error);
int32_t sapflow_media_sample_metadata(const void *sample,
    SapflowMediaSampleResult *out, SapflowBuffer *error);
int32_t sapflow_aac_timing_views_sample(const void *views, bool publishing,
    SapflowMediaSampleResult *out, SapflowBuffer *error);
int32_t sapflow_sample_fanout_push(const void *fanout, const void *sample,
    uint64_t now_ns, SapflowFanoutOutcome *out, SapflowBuffer *error);
int32_t sapflow_sample_fanout_push_aac_timing_views(const void *fanout,
    const void *views, uint64_t now_ns, SapflowFanoutOutcome *out,
    SapflowBuffer *error);
int32_t sapflow_sample_fanout_pop(const void *fanout, bool recording,
    uint64_t now_ns, SapflowMediaSampleResult *out, SapflowBuffer *error);
int32_t sapflow_recording_muxer_push(const void *muxer, const void *sample,
    SapflowBuffer *error);
int32_t sapflow_recording_muxer_take_output(const void *muxer,
    SapflowRecordingOutput *out, SapflowBuffer *error);
int32_t sapflow_recording_muxer_take_native_output(const void *muxer,
    SapflowRecordingOutput *out, SapflowBuffer *error);
int32_t sapflow_recording_muxer_take_programme_output(const void *muxer,
    SapflowRecordingOutput *out, SapflowBuffer *error);
int32_t sapflow_rtmp_publisher_feed(const void *publisher,
    const uint8_t *bytes, size_t len, uint64_t now_ms, SapflowBuffer *error);
int32_t sapflow_rtmp_publisher_send(const void *publisher, const void *sample,
    SapflowBuffer *error);
int32_t sapflow_rtmp_publisher_take_output(const void *publisher,
    SapflowBuffer *out, SapflowBuffer *error);
int32_t sapflow_rtmp_publisher_send_and_take_output(const void *publisher,
    const void *sample, uint64_t now_ms, SapflowRtmpSendOutput *out,
    SapflowBuffer *error);
int32_t sapflow_rtmp_publisher_next_deadline_ms(const void *publisher,
    SapflowOptionalU64 *out, SapflowBuffer *error);
int32_t sapflow_rtmp_publisher_queued_output_bytes(const void *publisher,
    uint64_t *out, SapflowBuffer *error);
int32_t sapflow_rtmp_publisher_state(const void *publisher, uint64_t *out,
    SapflowBuffer *error);
int32_t sapflow_srt_publisher_push(const void *publisher, const void *sample,
    uint64_t now_us, SapflowBuffer *error);
int32_t sapflow_srt_publisher_feed(const void *publisher,
    const uint8_t *source, size_t source_len, const uint8_t *bytes, size_t len,
    uint64_t now_us, SapflowBuffer *error);
int32_t sapflow_srt_publisher_poll_datagram(const void *publisher,
    uint64_t now_us, SapflowSrtDatagram *out, SapflowBuffer *error);
int32_t sapflow_srt_publisher_is_ready(const void *publisher, uint64_t *out,
    SapflowBuffer *error);
int32_t sapflow_srt_publisher_has_immediate_work(const void *publisher,
    uint64_t *out, SapflowBuffer *error);
int32_t sapflow_srt_publisher_next_deadline_us(const void *publisher,
    SapflowOptionalU64 *out, SapflowBuffer *error);
int32_t sapflow_srt_publisher_state(const void *publisher, uint64_t *out,
    SapflowBuffer *error);

#ifdef __cplusplus
}
#endif
#endif
