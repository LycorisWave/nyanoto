// Thin C wrapper over Signalsmith Stretch. C++ exceptions never cross into Rust.

#include <signalsmith-stretch.h>

#include <cstddef>
#include <new>

namespace {

// Чередующиеся каналы в виде, который ждёт Signalsmith: buffer[канал][кадр].
class Interleaved {
public:
    Interleaved(float *data, int channels) : data(data), channels(channels) {}

    class Channel {
    public:
        Channel(float *data, int channel, int stride) : data(data), channel(channel), stride(stride) {}
        float &operator[](int frame) { return data[frame * stride + channel]; }
        const float &operator[](int frame) const { return data[frame * stride + channel]; }

    private:
        float *data;
        int channel;
        int stride;
    };

    Channel operator[](int channel) { return Channel(data, channel, channels); }

private:
    float *data;
    int channels;
};

struct Stretch {
    signalsmith::stretch::SignalsmithStretch<float> inner;
    int channels;
};

} // namespace

extern "C" {

void *nyanoto_stretch_new(int channels, float sample_rate) {
    try {
        auto stretch = new Stretch;
        stretch->channels = channels;
        stretch->inner.presetDefault(channels, sample_rate);
        return stretch;
    } catch (...) {
        return nullptr;
    }
}

void nyanoto_stretch_free(void *handle) {
    delete static_cast<Stretch *>(handle);
}

int nyanoto_stretch_input_latency(void *handle) {
    return static_cast<Stretch *>(handle)->inner.inputLatency();
}

int nyanoto_stretch_output_latency(void *handle) {
    return static_cast<Stretch *>(handle)->inner.outputLatency();
}

int nyanoto_stretch_seek(void *handle, float *input, int frames, double rate) {
    try {
        auto stretch = static_cast<Stretch *>(handle);
        stretch->inner.seek(Interleaved(input, stretch->channels), frames, rate);
        return 1;
    } catch (...) {
        return 0;
    }
}

int nyanoto_stretch_process(void *handle, float *input, int input_frames, float *output, int output_frames) {
    try {
        auto stretch = static_cast<Stretch *>(handle);
        stretch->inner.process(Interleaved(input, stretch->channels), input_frames,
                               Interleaved(output, stretch->channels), output_frames);
        return 1;
    } catch (...) {
        return 0;
    }
}

int nyanoto_stretch_flush(void *handle, float *output, int output_frames) {
    try {
        auto stretch = static_cast<Stretch *>(handle);
        stretch->inner.flush(Interleaved(output, stretch->channels), output_frames);
        return 1;
    } catch (...) {
        return 0;
    }
}

} // extern "C"
