/* Host-only ABI adapter. The codec is the unmodified, pinned AOSP implementation. */
#include <stdlib.h>
#include "oi_codec_sbc.h"

struct decoder {
    OI_CODEC_SBC_DECODER_CONTEXT context;
    uint32_t data[CODEC_DATA_WORDS(2, SBC_CODEC_FAST_FILTER_BUFFERS)];
};

void *oracle_decoder_new(int channels, int msbc) {
    struct decoder *decoder = calloc(1, sizeof(*decoder));
    if (!decoder) return NULL;
    if (OI_CODEC_SBC_DecoderReset(&decoder->context, decoder->data, sizeof(decoder->data), 2, channels, 0)) {
        free(decoder);
        return NULL;
    }
    if (msbc && OI_CODEC_SBC_DecoderConfigureMSbc(&decoder->context)) {
        free(decoder);
        return NULL;
    }
    return decoder;
}

int oracle_decode(void *state, const uint8_t *frame, uint32_t length, int16_t *pcm, uint32_t bytes) {
    struct decoder *decoder = state;
    int status = OI_CODEC_SBC_DecodeFrame(&decoder->context, &frame, &length, pcm, &bytes);
    if (status) return -status;
    return length ? -1 : (int)bytes;
}

void oracle_decoder_free(void *context) { free(context); }
