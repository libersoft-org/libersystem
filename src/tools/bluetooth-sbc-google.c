/* Host-only adapter for the independent Google SBC decoder. It covers the
   AOSP decoder's proven four-subband joint-stereo bit-reader defect. */
#include <stdlib.h>
#include "sbc.h"

void *google_sbc_new(void) {
    sbc_t *context = calloc(1, sizeof(*context));
    if (context) sbc_reset(context);
    return context;
}

int google_sbc_decode(void *context, const void *data, unsigned size, int16_t *pcm) {
    struct sbc_frame frame;
    if (sbc_probe(data, &frame)) return -1;
    int channels = frame.mode == SBC_MODE_MONO ? 1 : 2;
    if (sbc_decode(context, data, size, &frame, pcm, channels, pcm + channels - 1, channels)) return -1;
    return frame.nblocks * frame.nsubbands * channels * sizeof(*pcm);
}

void google_sbc_free(void *context) { free(context); }
