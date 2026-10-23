// FFmpeg shim for native/ffmpeg.rs. Not linked: loaded on the first file that needs it.
// Built against vendor/ffmpeg 7.1 headers, other majors refused (struct layouts change).
// English on purpose: MSVC may read C files in the local code page.

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/channel_layout.h>
#include <libavutil/dict.h>
#include <libavutil/mem.h>
#include <libavutil/samplefmt.h>

#ifdef _WIN32
#include <windows.h>
#else
#include <dlfcn.h>
#endif

#define STR2(x) #x
#define STR(x) STR2(x)

#if defined(_WIN32)
#define LIB_UTIL "avutil-" STR(LIBAVUTIL_VERSION_MAJOR) ".dll"
#define LIB_CODEC "avcodec-" STR(LIBAVCODEC_VERSION_MAJOR) ".dll"
#define LIB_FORMAT "avformat-" STR(LIBAVFORMAT_VERSION_MAJOR) ".dll"
#define SEPARATOR "\\"
#elif defined(__ANDROID__)
#define LIB_UTIL "libavutil.so"
#define LIB_CODEC "libavcodec.so"
#define LIB_FORMAT "libavformat.so"
#define SEPARATOR "/"
#else
#define LIB_UTIL "libavutil.so." STR(LIBAVUTIL_VERSION_MAJOR)
#define LIB_CODEC "libavcodec.so." STR(LIBAVCODEC_VERSION_MAJOR)
#define LIB_FORMAT "libavformat.so." STR(LIBAVFORMAT_VERSION_MAJOR)
#define SEPARATOR "/"
#endif

typedef struct NyanotoFfIo {
    void *user;
    // bytes read, 0 at the end, negative on error
    int64_t (*read)(void *user, uint8_t *buffer, int64_t bytes);
    // new position or negative on error; whence: 0 begin, 1 current, 2 end
    int64_t (*seek)(void *user, int64_t offset, int whence);
    int64_t (*size)(void *user);
} NyanotoFfIo;

typedef struct NyanotoFfInfo {
    int64_t frames;          // samples per channel, 0 if unknown
    int32_t sample_rate;
    int32_t channels;
    int32_t bits;            // bits_per_raw_sample, 0 if not meaningful
    int32_t lossless;
    uint64_t channel_mask;   // native-order mask, 0 if none
    int64_t bit_rate;
    char codec[32];
    char profile[48];
    char container[32];
} NyanotoFfInfo;

static struct {
    int tried;
    int loaded;
    char error[512];
    char where[512];
    void *util, *codec, *format;

    unsigned (*avutil_version)(void);
    unsigned (*avcodec_version)(void);
    unsigned (*avformat_version)(void);
    void (*av_log_set_level)(int);
    void *(*av_malloc)(size_t);
    void (*av_freep)(void *);
    const AVDictionaryEntry *(*av_dict_iterate)(const AVDictionary *, const AVDictionaryEntry *);
    int64_t (*av_rescale_q)(int64_t, AVRational, AVRational);
    int (*av_sample_fmt_is_planar)(enum AVSampleFormat);
    AVFrame *(*av_frame_alloc)(void);
    void (*av_frame_free)(AVFrame **);
    void (*av_frame_unref)(AVFrame *);

    const AVCodec *(*avcodec_find_decoder)(enum AVCodecID);
    const AVCodecDescriptor *(*avcodec_descriptor_get)(enum AVCodecID);
    const char *(*avcodec_get_name)(enum AVCodecID);
    const char *(*avcodec_profile_name)(enum AVCodecID, int);
    AVCodecContext *(*avcodec_alloc_context3)(const AVCodec *);
    void (*avcodec_free_context)(AVCodecContext **);
    int (*avcodec_parameters_to_context)(AVCodecContext *, const AVCodecParameters *);
    int (*avcodec_open2)(AVCodecContext *, const AVCodec *, AVDictionary **);
    int (*avcodec_send_packet)(AVCodecContext *, const AVPacket *);
    int (*avcodec_receive_frame)(AVCodecContext *, AVFrame *);
    void (*avcodec_flush_buffers)(AVCodecContext *);
    AVPacket *(*av_packet_alloc)(void);
    void (*av_packet_free)(AVPacket **);
    void (*av_packet_unref)(AVPacket *);

    AVFormatContext *(*avformat_alloc_context)(void);
    int (*avformat_open_input)(AVFormatContext **, const char *, const AVInputFormat *, AVDictionary **);
    int (*avformat_find_stream_info)(AVFormatContext *, AVDictionary **);
    int (*av_find_best_stream)(AVFormatContext *, enum AVMediaType, int, int, const AVCodec **, int);
    void (*avformat_close_input)(AVFormatContext **);
    int (*av_read_frame)(AVFormatContext *, AVPacket *);
    int (*av_seek_frame)(AVFormatContext *, int, int64_t, int);
    AVIOContext *(*avio_alloc_context)(unsigned char *, int, int, void *,
        int (*)(void *, uint8_t *, int), int (*)(void *, const uint8_t *, int), int64_t (*)(void *, int64_t, int));
    void (*avio_context_free)(AVIOContext **);
} ff;

static void *open_library(const char *dir, const char *name)
{
    char path[1024];
    if (dir && *dir)
        snprintf(path, sizeof path, "%s" SEPARATOR "%s", dir, name);
    else
        snprintf(path, sizeof path, "%s", name);
#ifdef _WIN32
    // UTF-8 paths go to the wide API. A full path lets the DLL find its neighbours.
    wchar_t wide[1024];
    if (!MultiByteToWideChar(CP_UTF8, 0, path, -1, wide, (int) (sizeof wide / sizeof wide[0])))
        return NULL;
    HMODULE module = LoadLibraryExW(wide, NULL, (dir && *dir) ? LOAD_WITH_ALTERED_SEARCH_PATH : 0);
    return (void *) module;
#else
    return dlopen(path, RTLD_NOW | RTLD_LOCAL);
#endif
}

static void close_library(void *library)
{
    if (!library)
        return;
#ifdef _WIN32
    FreeLibrary((HMODULE) library);
#else
    dlclose(library);
#endif
}

static void *symbol(void *library, const char *name)
{
#ifdef _WIN32
    return (void *) GetProcAddress((HMODULE) library, name);
#else
    return dlsym(library, name);
#endif
}

#define LOAD(library, name)                                                    \
    do {                                                                       \
        *(void **) &ff.name = symbol(ff.library, #name);                       \
        if (!ff.name) {                                                        \
            snprintf(ff.error, sizeof ff.error, "no %s in %s", #name, where);  \
            return 0;                                                          \
        }                                                                      \
    } while (0)

static int load_from(const char *dir)
{
    const char *where = (dir && *dir) ? dir : "system paths";
    ff.util = open_library(dir, LIB_UTIL);
    ff.codec = ff.util ? open_library(dir, LIB_CODEC) : NULL;
    ff.format = ff.codec ? open_library(dir, LIB_FORMAT) : NULL;
    if (!ff.format) {
        snprintf(ff.error, sizeof ff.error, "%s, %s, %s not found in %s", LIB_UTIL, LIB_CODEC, LIB_FORMAT, where);
        return 0;
    }
    LOAD(util, avutil_version);
    LOAD(codec, avcodec_version);
    LOAD(format, avformat_version);
    if ((ff.avutil_version() >> 16) != LIBAVUTIL_VERSION_MAJOR ||
        (ff.avcodec_version() >> 16) != LIBAVCODEC_VERSION_MAJOR ||
        (ff.avformat_version() >> 16) != LIBAVFORMAT_VERSION_MAJOR) {
        snprintf(ff.error, sizeof ff.error, "FFmpeg in %s is %u.%u, need major %d", where,
            ff.avcodec_version() >> 16, (ff.avcodec_version() >> 8) & 0xff, LIBAVCODEC_VERSION_MAJOR);
        return 0;
    }
    LOAD(util, av_log_set_level);
    LOAD(util, av_malloc);
    LOAD(util, av_freep);
    LOAD(util, av_dict_iterate);
    LOAD(util, av_rescale_q);
    LOAD(util, av_sample_fmt_is_planar);
    LOAD(util, av_frame_alloc);
    LOAD(util, av_frame_free);
    LOAD(util, av_frame_unref);
    LOAD(codec, avcodec_find_decoder);
    LOAD(codec, avcodec_descriptor_get);
    LOAD(codec, avcodec_get_name);
    LOAD(codec, avcodec_profile_name);
    LOAD(codec, avcodec_alloc_context3);
    LOAD(codec, avcodec_free_context);
    LOAD(codec, avcodec_parameters_to_context);
    LOAD(codec, avcodec_open2);
    LOAD(codec, avcodec_send_packet);
    LOAD(codec, avcodec_receive_frame);
    LOAD(codec, avcodec_flush_buffers);
    LOAD(codec, av_packet_alloc);
    LOAD(codec, av_packet_free);
    LOAD(codec, av_packet_unref);
    LOAD(format, avformat_alloc_context);
    LOAD(format, avformat_open_input);
    LOAD(format, avformat_find_stream_info);
    LOAD(format, av_find_best_stream);
    LOAD(format, avformat_close_input);
    LOAD(format, av_read_frame);
    LOAD(format, av_seek_frame);
    LOAD(format, avio_alloc_context);
    LOAD(format, avio_context_free);
    snprintf(ff.where, sizeof ff.where, "%s", where);
    return 1;
}

// Loads once. `dirs` are '\n'-separated, then system paths. Not thread-safe, Rust locks.
int nyanoto_ff_load(const char *dirs)
{
    if (ff.tried)
        return ff.loaded;
    ff.tried = 1;
    char list[4096];
    snprintf(list, sizeof list, "%s", dirs ? dirs : "");
    char *start = list;
    for (;;) {
        char *end = strchr(start, '\n');
        if (end)
            *end = 0;
        if (*start && load_from(start))
            break;
        close_library(ff.format);
        close_library(ff.codec);
        close_library(ff.util);
        ff.format = ff.codec = ff.util = NULL;
        if (!end) {
            if (!load_from(""))
                return 0;
            break;
        }
        start = end + 1;
    }
    ff.av_log_set_level(AV_LOG_QUIET);
    ff.loaded = 1;
    ff.error[0] = 0;
    return 1;
}

const char *nyanoto_ff_error(void) { return ff.error; }
const char *nyanoto_ff_where(void) { return ff.where; }
unsigned nyanoto_ff_version(void) { return ff.loaded ? ff.avcodec_version() : 0; }

typedef struct Decoder {
    NyanotoFfIo io;
    AVFormatContext *format;
    AVIOContext *avio;
    AVCodecContext *codec;
    AVPacket *packet;
    AVFrame *frame;
    int stream;
    int channels;
    int rate;
    AVRational time_base;
    int64_t start_time;
    int draining;
    int ended;
    // after a seek: samples before this one are dropped (-1: nothing to drop)
    int64_t discard_until;
    float *buffer;
    int64_t capacity;
} Decoder;

static int read_callback(void *opaque, uint8_t *buffer, int size)
{
    Decoder *decoder = opaque;
    int64_t read = decoder->io.read(decoder->io.user, buffer, size);
    if (read < 0)
        return AVERROR(EIO);
    if (read == 0)
        return AVERROR_EOF;
    return (int) read;
}

static int64_t seek_callback(void *opaque, int64_t offset, int whence)
{
    Decoder *decoder = opaque;
    if (whence & AVSEEK_SIZE)
        return decoder->io.size(decoder->io.user);
    return decoder->io.seek(decoder->io.user, offset, whence & 3);
}

void nyanoto_ff_close(void *handle)
{
    Decoder *decoder = handle;
    if (!decoder)
        return;
    if (decoder->codec)
        ff.avcodec_free_context(&decoder->codec);
    if (decoder->format)
        ff.avformat_close_input(&decoder->format);
    if (decoder->avio) {
        ff.av_freep(&decoder->avio->buffer);
        ff.avio_context_free(&decoder->avio);
    }
    if (decoder->packet)
        ff.av_packet_free(&decoder->packet);
    if (decoder->frame)
        ff.av_frame_free(&decoder->frame);
    ff.av_freep(&decoder->buffer);
    ff.av_freep(&decoder);
}

static int is_lossless(const AVCodecParameters *par)
{
    const AVCodecDescriptor *descriptor = ff.avcodec_descriptor_get(par->codec_id);
    if (par->codec_id == AV_CODEC_ID_DTS)
        return par->profile == AV_PROFILE_DTS_HD_MA || par->profile == AV_PROFILE_DTS_HD_MA_X ||
            par->profile == AV_PROFILE_DTS_HD_MA_X_IMAX;
    return descriptor && (descriptor->props & AV_CODEC_PROP_LOSSLESS) && !(descriptor->props & AV_CODEC_PROP_LOSSY);
}

// Opens a file; NULL and a message in `error` on failure.
void *nyanoto_ff_open(const NyanotoFfIo *io, NyanotoFfInfo *info, char *error, int error_size)
{
    if (!ff.loaded) {
        snprintf(error, error_size, "FFmpeg not loaded");
        return NULL;
    }
    Decoder *decoder = ff.av_malloc(sizeof *decoder);
    if (!decoder) {
        snprintf(error, error_size, "out of memory");
        return NULL;
    }
    memset(decoder, 0, sizeof *decoder);
    decoder->io = *io;
    decoder->discard_until = -1;
    const int avio_size = 32768;
    unsigned char *avio_buffer = ff.av_malloc(avio_size);
    decoder->avio = avio_buffer ? ff.avio_alloc_context(avio_buffer, avio_size, 0, decoder, read_callback, NULL, seek_callback) : NULL;
    decoder->format = ff.avformat_alloc_context();
    decoder->packet = ff.av_packet_alloc();
    decoder->frame = ff.av_frame_alloc();
    if (!decoder->avio || !decoder->format || !decoder->packet || !decoder->frame) {
        if (!decoder->avio)
            ff.av_freep(&avio_buffer);
        snprintf(error, error_size, "out of memory");
        nyanoto_ff_close(decoder);
        return NULL;
    }
    decoder->format->pb = decoder->avio;
    int result = ff.avformat_open_input(&decoder->format, NULL, NULL, NULL);
    if (result < 0) {
        // avformat_open_input frees the context on failure
        decoder->format = NULL;
        snprintf(error, error_size, "unknown container (%d)", result);
        nyanoto_ff_close(decoder);
        return NULL;
    }
    ff.avformat_find_stream_info(decoder->format, NULL);
    const AVCodec *codec = NULL;
    decoder->stream = ff.av_find_best_stream(decoder->format, AVMEDIA_TYPE_AUDIO, -1, -1, &codec, 0);
    if (decoder->stream < 0 || !codec) {
        snprintf(error, error_size, decoder->stream == AVERROR_DECODER_NOT_FOUND ? "no decoder for the codec" : "no audio stream");
        nyanoto_ff_close(decoder);
        return NULL;
    }
    AVStream *stream = decoder->format->streams[decoder->stream];
    AVCodecParameters *par = stream->codecpar;
    decoder->codec = ff.avcodec_alloc_context3(codec);
    if (!decoder->codec || ff.avcodec_parameters_to_context(decoder->codec, par) < 0 ||
        ff.avcodec_open2(decoder->codec, codec, NULL) < 0) {
        snprintf(error, error_size, "decoder %s does not open", ff.avcodec_get_name(par->codec_id));
        nyanoto_ff_close(decoder);
        return NULL;
    }
    decoder->channels = decoder->codec->ch_layout.nb_channels > 0 ? decoder->codec->ch_layout.nb_channels : par->ch_layout.nb_channels;
    decoder->rate = decoder->codec->sample_rate > 0 ? decoder->codec->sample_rate : par->sample_rate;
    decoder->time_base = stream->time_base;
    decoder->start_time = stream->start_time == AV_NOPTS_VALUE ? 0 : stream->start_time;
    if (decoder->channels <= 0 || decoder->rate <= 0) {
        snprintf(error, error_size, "stream without channels or rate");
        nyanoto_ff_close(decoder);
        return NULL;
    }

    memset(info, 0, sizeof *info);
    info->sample_rate = decoder->rate;
    info->channels = decoder->channels;
    info->bits = par->bits_per_raw_sample;
    info->lossless = is_lossless(par);
    info->bit_rate = par->bit_rate > 0 ? par->bit_rate : decoder->format->bit_rate;
    const AVChannelLayout *layout = decoder->codec->ch_layout.nb_channels > 0 ? &decoder->codec->ch_layout : &par->ch_layout;
    if (layout->order == AV_CHANNEL_ORDER_NATIVE)
        info->channel_mask = layout->u.mask;
    if (stream->duration != AV_NOPTS_VALUE && stream->duration > 0)
        info->frames = ff.av_rescale_q(stream->duration, stream->time_base, (AVRational) { 1, decoder->rate });
    else if (decoder->format->duration != AV_NOPTS_VALUE && decoder->format->duration > 0)
        info->frames = ff.av_rescale_q(decoder->format->duration, AV_TIME_BASE_Q, (AVRational) { 1, decoder->rate });
    snprintf(info->codec, sizeof info->codec, "%s", ff.avcodec_get_name(par->codec_id));
    const char *profile = ff.avcodec_profile_name(par->codec_id, par->profile);
    snprintf(info->profile, sizeof info->profile, "%s", profile ? profile : "");
    snprintf(info->container, sizeof info->container, "%s", decoder->format->iformat ? decoder->format->iformat->name : "");
    return decoder;
}

// Metadata of the file, then of the audio stream: the `index`-th entry. 0 when there is none.
int nyanoto_ff_tag(void *handle, int index, const char **key, const char **value)
{
    Decoder *decoder = handle;
    const AVDictionary *dicts[2] = { decoder->format->metadata, decoder->format->streams[decoder->stream]->metadata };
    for (int d = 0; d < 2; d++) {
        const AVDictionaryEntry *entry = NULL;
        while ((entry = ff.av_dict_iterate(dicts[d], entry))) {
            if (index-- == 0) {
                *key = entry->key;
                *value = entry->value;
                return 1;
            }
        }
    }
    return 0;
}

static float sample_at(const AVFrame *frame, int channel, int index, int channels)
{
    const int planar = ff.av_sample_fmt_is_planar(frame->format);
    const uint8_t *data = planar ? frame->extended_data[channel] : frame->extended_data[0];
    const int position = planar ? index : index * channels + channel;
    switch (frame->format) {
    case AV_SAMPLE_FMT_U8:
    case AV_SAMPLE_FMT_U8P:
        return ((int) data[position] - 128) / 128.0f;
    case AV_SAMPLE_FMT_S16:
    case AV_SAMPLE_FMT_S16P:
        return ((const int16_t *) data)[position] / 32768.0f;
    case AV_SAMPLE_FMT_S32:
    case AV_SAMPLE_FMT_S32P:
        return (float) (((const int32_t *) data)[position] / 2147483648.0);
    case AV_SAMPLE_FMT_S64:
    case AV_SAMPLE_FMT_S64P:
        return (float) (((const int64_t *) data)[position] / 9223372036854775808.0);
    case AV_SAMPLE_FMT_FLT:
    case AV_SAMPLE_FMT_FLTP:
        return ((const float *) data)[position];
    case AV_SAMPLE_FMT_DBL:
    case AV_SAMPLE_FMT_DBLP:
        return (float) ((const double *) data)[position];
    default:
        return 0.0f;
    }
}

// Frames decoded (interleaved in *out), 0 at the end. `damaged` counts bad packets.
int64_t nyanoto_ff_read(void *handle, const float **out, int *damaged)
{
    Decoder *decoder = handle;
    *damaged = 0;
    for (int guard = 0; guard < 100000; guard++) {
        if (decoder->ended)
            return 0;
        int result = ff.avcodec_receive_frame(decoder->codec, decoder->frame);
        if (result == 0) {
            AVFrame *frame = decoder->frame;
            int64_t count = frame->nb_samples;
            int64_t skip = 0;
            if (decoder->discard_until >= 0) {
                int64_t pts = frame->best_effort_timestamp != AV_NOPTS_VALUE ? frame->best_effort_timestamp : frame->pts;
                if (pts != AV_NOPTS_VALUE) {
                    int64_t first = ff.av_rescale_q(pts - decoder->start_time, decoder->time_base, (AVRational) { 1, decoder->rate });
                    skip = decoder->discard_until - first;
                    if (skip < 0)
                        skip = 0;
                }
                if (skip >= count) {
                    ff.av_frame_unref(frame);
                    continue;
                }
                decoder->discard_until = -1;
            }
            const int64_t frames = count - skip;
            const int channels = decoder->channels;
            const int have = frame->ch_layout.nb_channels;
            if (frames * channels > decoder->capacity) {
                ff.av_freep(&decoder->buffer);
                decoder->capacity = frames * channels;
                decoder->buffer = ff.av_malloc(sizeof(float) * (size_t) decoder->capacity);
                if (!decoder->buffer) {
                    decoder->capacity = 0;
                    ff.av_frame_unref(frame);
                    return -1;
                }
            }
            // Channel count changed mid-stream: keep the first, drop extra, silence missing.
            for (int64_t index = 0; index < frames; index++)
                for (int channel = 0; channel < channels; channel++)
                    decoder->buffer[index * channels + channel] =
                        channel < have ? sample_at(frame, channel, (int) (index + skip), have) : 0.0f;
            ff.av_frame_unref(frame);
            *out = decoder->buffer;
            return frames;
        }
        if (result == AVERROR_EOF) {
            decoder->ended = 1;
            return 0;
        }
        if (result != AVERROR(EAGAIN)) {
            (*damaged)++;
            continue;
        }
        if (decoder->draining) {
            decoder->ended = 1;
            return 0;
        }
        result = ff.av_read_frame(decoder->format, decoder->packet);
        if (result < 0) {
            // end of file (or unreadable rest): flush the decoder
            decoder->draining = 1;
            ff.avcodec_send_packet(decoder->codec, NULL);
            continue;
        }
        if (decoder->packet->stream_index == decoder->stream) {
            int sent = ff.avcodec_send_packet(decoder->codec, decoder->packet);
            if (sent < 0) {
                (*damaged)++;
            }
        }
        ff.av_packet_unref(decoder->packet);
    }
    return -1;
}

// Seeks so that the next frame returned is sample `frame`. 0 on success.
int nyanoto_ff_seek(void *handle, int64_t frame)
{
    Decoder *decoder = handle;
    int64_t target = ff.av_rescale_q(frame, (AVRational) { 1, decoder->rate }, decoder->time_base) + decoder->start_time;
    if (ff.av_seek_frame(decoder->format, decoder->stream, target, AVSEEK_FLAG_BACKWARD) < 0 &&
        ff.av_seek_frame(decoder->format, decoder->stream, target, AVSEEK_FLAG_BACKWARD | AVSEEK_FLAG_ANY) < 0)
        return -1;
    ff.avcodec_flush_buffers(decoder->codec);
    decoder->draining = 0;
    decoder->ended = 0;
    decoder->discard_until = frame;
    return 0;
}
