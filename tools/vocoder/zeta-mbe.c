/* SPDX-License-Identifier: Apache-2.0
 *
 * zeta-mbe: the speech decoder a ZetaSDR node pipes P25 and DMR voice frames to.
 *
 * This file is part of ZetaSDR and carries its licence. It holds no codec: it is the glue
 * between the node's frames and the operator's own copy of mbelib-neo, which
 * tools/install-vocoders.sh downloads and links into it on the operator's machine. Neither
 * cargo nor CI compiles this file. docs/vocoders.md says why.
 *
 *   zeta-mbe imbe7200x4400   P25 phase 1 full rate (IMBE): 184 bytes in per frame
 *   zeta-mbe ambe3600x2450   DMR (AMBE+2): 96 bytes in per frame
 *   zeta-mbe --version       this program's version and the library's
 *
 * Input, on stdin: one frame is the codec's channel-coded frame after the protocol's own
 * deinterleaving, row by row as the library takes it (IMBE 8 rows of 23, AMBE 4 rows of 24),
 * one byte per bit: 0 a certain zero, 255 a certain one, values between the demodulator's
 * confidence. The codec's own error correction runs in the library.
 *
 * Output, on stdout: 160 samples of signed 16-bit little-endian speech at 8 kHz per frame,
 * written as soon as the frame is decoded. Nothing is held across frames: the node expects
 * each frame's speech promptly and stops a program that takes frames and answers nothing.
 *
 * Soft decisions: the pinned mbelib-neo (2.6.11) has soft-decision frame functions, and this
 * program uses them. Each byte becomes a hard bit (128 and over is a one) with a reliability
 * of its distance from the midpoint, so 0 and 255 are fully reliable and 127 and 128 are
 * nearly erasures.
 *
 * Level: the library's float frame functions return speech at about 1/7 of 16-bit full
 * scale, and its 16-bit functions multiply by 7, which clips loud speech. This program
 * scales the float speech by ZETA_MBE_GAIN and saturates at the 16-bit limits itself.
 *
 * End of input is a clean exit (status 0). A bad argument prints the usage (status 2).
 */

#include <errno.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <mbelib-neo/mbelib.h>

#define ZETA_MBE_VERSION "1.0.0"

/* Samples per frame out: 20 ms at 8 kHz, both codecs. */
#define MBE_FRAME_SAMPLES 160

/* Bytes per frame in: one per bit of the channel-coded frame. */
#define IMBE_ROWS 8
#define IMBE_COLS 23
#define IMBE_FEED_BYTES (IMBE_ROWS * IMBE_COLS) /* 184 */
#define AMBE_ROWS 4
#define AMBE_COLS 24
#define AMBE_FEED_BYTES (AMBE_ROWS * AMBE_COLS) /* 96 */

/* An input byte at or above this is a one. */
#define SOFT_MIDPOINT 128

/* Float speech to 16-bit: below the library's own factor of 7, so loud speech keeps its
 * peaks instead of meeting the limit. */
#define ZETA_MBE_GAIN 4.0f

enum codec { CODEC_IMBE, CODEC_AMBE };

static void usage(void) {
    fprintf(stderr,
            "usage: zeta-mbe <codec>\n"
            "       zeta-mbe --version\n"
            "codecs:\n"
            "  imbe7200x4400   P25 phase 1 full rate, %d bytes in per frame\n"
            "  ambe3600x2450   DMR AMBE+2, %d bytes in per frame\n"
            "One byte per bit in on stdin (0 a certain zero, 255 a certain one);\n"
            "%d samples of s16le at 8 kHz out on stdout per frame.\n",
            IMBE_FEED_BYTES, AMBE_FEED_BYTES, MBE_FRAME_SAMPLES);
}

/* One input byte as the library's soft bit: the hard decision and its reliability. */
static mbe_soft_bit soft_bit(uint8_t byte) {
    mbe_soft_bit s;
    if (byte >= SOFT_MIDPOINT) {
        s.bit = 1;
        s.reliability = (uint8_t)(2 * byte - 255);
    } else {
        s.bit = 0;
        s.reliability = (uint8_t)(255 - 2 * byte);
    }
    return s;
}

static int16_t to_s16(float x) {
    float y = x * ZETA_MBE_GAIN;
    if (!(y == y)) { /* NaN */
        return 0;
    }
    if (y >= 32767.0f) {
        return 32767;
    }
    if (y <= -32768.0f) {
        return -32768;
    }
    return (int16_t)lrintf(y);
}

/* Reads exactly n bytes. Returns n, or fewer at end of input; -1 on a read error. */
static long read_frame(uint8_t *buf, size_t n) {
    size_t got = 0;
    while (got < n) {
        size_t r = fread(buf + got, 1, n - got, stdin);
        if (r == 0) {
            if (ferror(stdin)) {
                if (errno == EINTR) {
                    clearerr(stdin);
                    continue;
                }
                return -1;
            }
            break;
        }
        got += r;
    }
    return (long)got;
}

/* Writes one frame of speech as s16le and flushes it. Returns 0, or -1 when stdout fails. */
static int write_speech(const float *speech) {
    uint8_t out[MBE_FRAME_SAMPLES * 2];
    for (int i = 0; i < MBE_FRAME_SAMPLES; i++) {
        uint16_t v = (uint16_t)to_s16(speech[i]);
        out[2 * i] = (uint8_t)(v & 0xff);
        out[2 * i + 1] = (uint8_t)(v >> 8);
    }
    if (fwrite(out, 1, sizeof out, stdout) != sizeof out) {
        return -1;
    }
    return fflush(stdout) == 0 ? 0 : -1;
}

static int run(enum codec codec) {
    const size_t feed = codec == CODEC_IMBE ? IMBE_FEED_BYTES : AMBE_FEED_BYTES;
    uint8_t in[IMBE_FEED_BYTES];
    mbe_soft_bit imbe_fr[IMBE_ROWS][IMBE_COLS];
    mbe_soft_bit ambe_fr[AMBE_ROWS][AMBE_COLS];
    char imbe_d[88];
    char ambe_d[49];
    float speech[MBE_FRAME_SAMPLES];
    mbe_parms cur, prev, prev_enhanced;
    int complained = 0;

    mbe_initMbeParms(&cur, &prev, &prev_enhanced);

    for (;;) {
        long got = read_frame(in, feed);
        if (got < 0) {
            fprintf(stderr, "zeta-mbe: reading stdin: %s\n", strerror(errno));
            return 1;
        }
        if ((size_t)got < feed) {
            if (got > 0) {
                fprintf(stderr, "zeta-mbe: input ended inside a frame (%ld of %zu bytes)\n",
                        got, feed);
            }
            return 0;
        }

        mbe_process_result result;
        mbe_initProcessResult(&result);
        int status;
        if (codec == CODEC_IMBE) {
            for (size_t i = 0; i < feed; i++) {
                imbe_fr[i / IMBE_COLS][i % IMBE_COLS] = soft_bit(in[i]);
            }
            status = mbe_processImbe7200x4400SoftFramef(speech, &result, imbe_fr, imbe_d, &cur,
                                                        &prev, &prev_enhanced);
        } else {
            for (size_t i = 0; i < feed; i++) {
                ambe_fr[i / AMBE_COLS][i % AMBE_COLS] = soft_bit(in[i]);
            }
            status = mbe_processAmbe3600x2450SoftFramef(speech, &result, ambe_fr, ambe_d, &cur,
                                                        &prev, &prev_enhanced);
        }
        if (status < 0) {
            /* The frame contract holds even then: this frame's speech is silence. */
            if (!complained) {
                fprintf(stderr, "zeta-mbe: the library refused a frame (status %d)\n", status);
                complained = 1;
            }
            memset(speech, 0, sizeof speech);
        }
        if (write_speech(speech) != 0) {
            return 1;
        }
    }
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);

    if (argc != 2) {
        usage();
        return 2;
    }
    if (strcmp(argv[1], "--version") == 0) {
        printf("zeta-mbe %s, mbelib-neo %s, soft-decision input\n", ZETA_MBE_VERSION,
               mbe_versionString());
        return 0;
    }
    if (strcmp(argv[1], "imbe7200x4400") == 0) {
        return run(CODEC_IMBE);
    }
    if (strcmp(argv[1], "ambe3600x2450") == 0) {
        return run(CODEC_AMBE);
    }
    usage();
    return 2;
}
