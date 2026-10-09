/* Host-only ABI adapter. The codec is the unmodified, pinned AOSP implementation. */
#include <stdlib.h>
#include "sbc_encoder.h"

void *oracle_encoder_new(int frequency, int blocks, int mode, int allocation, int bands, int pool, int msbc) {
    SBC_ENC_PARAMS *context = calloc(1, sizeof(*context));
    if (!context) return NULL;
    context->s16SamplingFreq = frequency;
    context->s16ChannelMode = mode;
    context->s16NumOfChannels = mode == SBC_MONO ? 1 : 2;
    context->s16NumOfBlocks = blocks;
    context->s16AllocationMethod = allocation;
    context->s16NumOfSubBands = bands;
    context->s16BitPool = pool;
    context->Format = msbc ? SBC_FORMAT_MSBC : SBC_FORMAT_GENERAL;
    SBC_Encoder_Init(context);
    /* AOSP's initializer derives a default pool from bitrate. The caller's
       negotiated bitpool is applied after initialization, as in its A2DP host. */
    context->s16BitPool = pool;
    return context;
}

int oracle_encode(void *context, int16_t *pcm, uint8_t *frame) {
    return SBC_Encode(context, pcm, frame);
}

void oracle_encoder_free(void *context) { free(context); }
