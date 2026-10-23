// C interface over the Monkey's Audio SDK for native/ape.rs.
// Read through Rust callbacks; reports CRC-damaged frames; MD5 like VerifyFileW2. Compression — tests only.
// English on purpose: MSVC may read C++ files in the local code page.

#include "All.h"
#include "MACLib.h"
#include "APEInfo.h"
#include "MD5.h"

#include <cstdint>
#include <cstdio>
#include <cstring>
#include <new>

using namespace APE;

extern "C" {

typedef struct NyanotoApeIo {
    void * user;
    // bytes read, or -1 on error
    int64_t (*read)(void * user, void * buffer, int64_t bytes);
    // bytes written, or -1 on error (only the tests write)
    int64_t (*write)(void * user, const void * buffer, int64_t bytes);
    // 0 on success; whence: 0 begin, 1 current, 2 end
    int (*seek)(void * user, int64_t position, int whence);
    int64_t (*position)(void * user);
    int64_t (*size)(void * user);
} NyanotoApeIo;

typedef struct NyanotoApeInfo {
    int64_t total_blocks;
    int32_t sample_rate;
    int32_t channels;
    int32_t bits;
    int32_t bytes_per_sample;
    int32_t version;
    int32_t format_flags;
    // WAVE_FORMAT_EXTENSIBLE channel mask from the stored WAV header, 0 if none
    uint32_t channel_mask;
} NyanotoApeInfo;

}

namespace {

class CallbackIO : public IAPEIO
{
public:
    explicit CallbackIO(const NyanotoApeIo & io) : m_io(io) { }

    int Open(const str_utfn *, bool) APE_OVERRIDE { return ERROR_SUCCESS; }
    int Close() APE_OVERRIDE { return ERROR_SUCCESS; }

    int Read(void * pBuffer, int64 nBytesToRead, int64 * pBytesRead) APE_OVERRIDE
    {
        const int64_t read = m_io.read(m_io.user, pBuffer, nBytesToRead);
        if (pBytesRead) *pBytesRead = read < 0 ? 0 : read;
        return read < 0 ? ERROR_IO_READ : ERROR_SUCCESS;
    }

    int Write(const void * pBuffer, int64 nBytesToWrite, int64 * pBytesWritten) APE_OVERRIDE
    {
        const int64_t written = m_io.write ? m_io.write(m_io.user, pBuffer, nBytesToWrite) : -1;
        if (pBytesWritten) *pBytesWritten = written < 0 ? 0 : written;
        return written == nBytesToWrite ? ERROR_SUCCESS : ERROR_IO_WRITE;
    }

    int Seek(int64 nPosition, SeekMethod nMethod) APE_OVERRIDE
    {
        return m_io.seek(m_io.user, nPosition, static_cast<int>(nMethod)) == 0 ? ERROR_SUCCESS : ERROR_IO_READ;
    }

    int Create(const str_utfn *) APE_OVERRIDE { return ERROR_SUCCESS; }
    int Delete() APE_OVERRIDE { return ERROR_UNDEFINED; }
    int SetEOF() APE_OVERRIDE { return ERROR_SUCCESS; }
    unsigned char * GetBuffer(int * pnBufferBytes) APE_OVERRIDE
    {
        if (pnBufferBytes) *pnBufferBytes = 0;
        return APE_NULL;
    }
    int64 GetPosition() APE_OVERRIDE { return m_io.position(m_io.user); }
    int64 GetSize() APE_OVERRIDE { return m_io.size(m_io.user); }

private:
    NyanotoApeIo m_io;
};

struct Decoder
{
    CallbackIO * io;
    IAPEDecompress * decompress;
};

// Channel mask from a WAVE_FORMAT_EXTENSIBLE "fmt " chunk inside the stored header.
uint32_t channel_mask(const unsigned char * header, int64 size)
{
    for (int64 at = 0; at + 8 <= size; at++)
    {
        if (memcmp(header + at, "fmt ", 4) != 0)
            continue;
        const int64 body = at + 8;
        if (body + 24 > size)
            return 0;
        const unsigned format = header[body] | (header[body + 1] << 8);
        if (format != 0xFFFE)
            return 0;
        return static_cast<uint32_t>(header[body + 20]) | (static_cast<uint32_t>(header[body + 21]) << 8) |
            (static_cast<uint32_t>(header[body + 22]) << 16) | (static_cast<uint32_t>(header[body + 23]) << 24);
    }
    return 0;
}

}

extern "C" {

// Opens a file; returns NULL and an SDK error code on failure.
void * nyanoto_ape_open(const NyanotoApeIo * io, NyanotoApeInfo * info, int * error)
{
    *error = ERROR_UNDEFINED;
    try
    {
        CallbackIO * callback = new CallbackIO(*io);
        int code = ERROR_UNDEFINED;
        IAPEDecompress * decompress = CreateIAPEDecompressEx(callback, &code);
        if (decompress == APE_NULL || code != ERROR_SUCCESS)
        {
            delete decompress;
            delete callback;
            *error = code;
            return APE_NULL;
        }
        info->total_blocks = decompress->GetInfo(IAPEDecompress::APE_DECOMPRESS_TOTAL_BLOCKS);
        info->sample_rate = static_cast<int32_t>(decompress->GetInfo(IAPEDecompress::APE_INFO_SAMPLE_RATE));
        info->channels = static_cast<int32_t>(decompress->GetInfo(IAPEDecompress::APE_INFO_CHANNELS));
        info->bits = static_cast<int32_t>(decompress->GetInfo(IAPEDecompress::APE_INFO_BITS_PER_SAMPLE));
        info->bytes_per_sample = static_cast<int32_t>(decompress->GetInfo(IAPEDecompress::APE_INFO_BYTES_PER_SAMPLE));
        info->version = static_cast<int32_t>(decompress->GetInfo(IAPEDecompress::APE_INFO_FILE_VERSION));
        info->format_flags = static_cast<int32_t>(decompress->GetInfo(IAPEDecompress::APE_INFO_FORMAT_FLAGS));
        info->channel_mask = 0;
        const int64 header_bytes = decompress->GetInfo(IAPEDecompress::APE_INFO_WAV_HEADER_BYTES);
        if (header_bytes > 0 && header_bytes < (1 << 20))
        {
            unsigned char * header = new unsigned char [static_cast<size_t>(header_bytes)];
            if (decompress->GetInfo(IAPEDecompress::APE_INFO_WAV_HEADER_DATA, APE_POINTER_TO_INT64(header), header_bytes) == ERROR_SUCCESS)
                info->channel_mask = channel_mask(header, header_bytes);
            delete [] header;
        }
        *error = ERROR_SUCCESS;
        return new Decoder { callback, decompress };
    }
    catch (...)
    {
        return APE_NULL;
    }
}

// Up to `blocks` blocks into `buffer` (WAV layout). `damaged` — a frame failed CRC. -1 — can't go on.
int64_t nyanoto_ape_read(void * handle, unsigned char * buffer, int64_t blocks, int * damaged)
{
    Decoder * decoder = static_cast<Decoder *>(handle);
    *damaged = 0;
    try
    {
        int64 retrieved = 0;
        const int result = decoder->decompress->GetData(buffer, blocks, &retrieved);
        if (result != ERROR_SUCCESS)
        {
            *damaged = 1;
            if (retrieved == 0)
                return -1;
        }
        return retrieved;
    }
    catch (...)
    {
        return -1;
    }
}

int nyanoto_ape_seek(void * handle, int64_t block)
{
    Decoder * decoder = static_cast<Decoder *>(handle);
    try
    {
        return decoder->decompress->Seek(block) == ERROR_SUCCESS ? 0 : -1;
    }
    catch (...)
    {
        return -1;
    }
}

// MD5 of the APE stream, as in the SDK's quick verify: 1 matches, 0 does not,
// -1 the file has none (encoder before 3.98, or an APL link).
int nyanoto_ape_md5(void * handle)
{
    Decoder * decoder = static_cast<Decoder *>(handle);
    try
    {
        IAPEDecompress * decompress = decoder->decompress;
        if (decompress->GetInfo(IAPEDecompress::APE_INFO_APL))
            return -1;
        const APE_FILE_INFO * info = decompress->GetFileInfo();
        if (info == APE_NULL || info->nVersionEncoder < 3980 || info->spAPEDescriptor == APE_NULL)
            return -1;
        IAPEIO * io = decompress->GetIO();
        const APE_DESCRIPTOR * descriptor = info->spAPEDescriptor;
        int64 read = 0;

        CSmartPtr<unsigned char> header(new unsigned char [descriptor->nHeaderBytes + 1], true);
        CSmartPtr<unsigned char> seek_table(new unsigned char [descriptor->nSeekTableBytes + 1], true);
        CSmartPtr<unsigned char> wav_header(new unsigned char [descriptor->nHeaderDataBytes + 1], true);
        io->Seek(static_cast<int64>(info->nJunkHeaderBytes) + static_cast<int64>(descriptor->nDescriptorBytes), SeekFileBegin);
        if (io->Read(header, descriptor->nHeaderBytes, &read) != ERROR_SUCCESS || read != descriptor->nHeaderBytes)
            return 0;
        if (io->Read(seek_table, descriptor->nSeekTableBytes, &read) != ERROR_SUCCESS || read != descriptor->nSeekTableBytes)
            return 0;
        if (io->Read(wav_header, descriptor->nHeaderDataBytes, &read) != ERROR_SUCCESS || read != descriptor->nHeaderDataBytes)
            return 0;

        CMD5Helper md5;
        md5.AddData(wav_header, descriptor->nHeaderDataBytes);
        int64 left = (static_cast<int64>(descriptor->nAPEFrameDataBytesHigh) << 32) +
            static_cast<int64>(descriptor->nAPEFrameDataBytes) + static_cast<int64>(descriptor->nTerminatingDataBytes);
        CSmartPtr<unsigned char> buffer(new unsigned char [16384], true);
        while (left > 0)
        {
            const int64 chunk = left < 16384 ? left : 16384;
            if (io->Read(buffer, chunk, &read) != ERROR_SUCCESS || read <= 0)
                return 0;
            md5.AddData(buffer, read);
            left -= read;
        }
        md5.AddData(header, descriptor->nHeaderBytes);
        md5.AddData(seek_table, descriptor->nSeekTableBytes);
        unsigned char result[16];
        md5.GetResult(result);
        const int matches = decompress->GetInfo(IAPEDecompress::APE_INFO_MD5_MATCHES, APE_POINTER_TO_INT64(&result[0])) == ERROR_SUCCESS;
        // The decoder keeps its own position; put the file back where it reads frames.
        decompress->Seek(decompress->GetInfo(IAPEDecompress::APE_DECOMPRESS_CURRENT_BLOCK));
        return matches;
    }
    catch (...)
    {
        return 0;
    }
}

// SDK version as "13.27".
const char * nyanoto_ape_version(void)
{
    static char version[16];
    snprintf(version, sizeof version, "%d.%02d", APE_VERSION_MAJOR, APE_VERSION_REVISION);
    return version;
}

void nyanoto_ape_close(void * handle)
{
    Decoder * decoder = static_cast<Decoder *>(handle);
    if (decoder == APE_NULL)
        return;
    delete decoder->decompress;
    delete decoder->io;
    delete decoder;
}

// For the tests: compresses WAV-layout PCM (or 32-bit float) into `out`.
// `header` is an optional WAV header to store (for the channel mask).
int nyanoto_ape_compress(const NyanotoApeIo * out, const unsigned char * pcm, int64_t bytes, int rate, int channels,
    int bits, int is_float, int level, const unsigned char * header, int64_t header_bytes)
{
    try
    {
        CallbackIO io(*out);
        int error = ERROR_SUCCESS;
        CSmartPtr<IAPECompress> compress(CreateIAPECompress(&error));
        if (compress == APE_NULL || error != ERROR_SUCCESS)
            return error ? error : ERROR_UNDEFINED;
        WAVEFORMATEX format;
        FillWaveFormatEx(&format, is_float ? WAVE_FORMAT_IEEE_FLOAT : WAVE_FORMAT_PCM, rate, bits, channels);
        error = compress->StartEx(&io, &format, is_float != 0, bytes, level, header,
            header ? header_bytes : CREATE_WAV_HEADER_ON_DECOMPRESSION);
        if (error != ERROR_SUCCESS)
            return error;
        if (compress->AddData(const_cast<unsigned char *>(pcm), bytes) != ERROR_SUCCESS)
            return ERROR_UNDEFINED;
        return compress->Finish(APE_NULL, 0, 0);
    }
    catch (...)
    {
        return ERROR_UNDEFINED;
    }
}

}
