/* DOOM (2016) stores virtual-texture page images as HD Photo codestreams with the image header
   stripped (the plane headers, an interleaved alpha plane and the macroblock data are intact).
   This rebuilds the header the engine's decoder assumes and decodes to RGBA8 with jxrlib. */
#include <stdlib.h>
#include <string.h>
#include "strcodec.h"

int jxr_decode_headerless(const unsigned char *data, size_t len, unsigned width, unsigned height,
                          unsigned overlap, int alpha, unsigned char *out, size_t stride)
{
    size_t total = 16 + len;
    unsigned char *buf = (unsigned char *)malloc(total);
    struct WMPStream *ws = NULL;
    CWMImageInfo ii;
    CWMIStrCodecParam scp;
    CTXSTRCODEC ctx = NULL;
    CWMImageBufferInfo bi;
    int rc = -1;

    if (!buf || width == 0 || height == 0 || width > 65536 || height > 65536)
        goto done;
    memcpy(buf, "WMPHOTO\0", 8);
    buf[8] = 0x10;                         /* codec version 1, sub-version 0 (original operators) */
    buf[9] = (unsigned char)(overlap & 3); /* no tiling, spatial order, no index table, overlap */
    buf[10] = 0xc0 | (alpha ? 1 : 0);      /* short header, long words, alpha image plane flag */
    buf[11] = 0x71;                        /* output colour format RGB, output bit depth 8 */
    buf[12] = (unsigned char)((width - 1) >> 8);
    buf[13] = (unsigned char)(width - 1);
    buf[14] = (unsigned char)((height - 1) >> 8);
    buf[15] = (unsigned char)(height - 1);
    memcpy(buf + 16, data, len);

    if (CreateWS_Memory(&ws, buf, total) != WMP_errSuccess)
        goto done;
    memset(&ii, 0, sizeof ii);
    memset(&scp, 0, sizeof scp);
    scp.pWStream = ws;
    if (ImageStrDecGetInfo(&ii, &scp) != ICERR_OK)
        goto done;
    ii.cfColorFormat = CF_RGB;
    ii.bdBitDepth = BD_8;
    ii.cBitsPerUnit = 32;
    ii.bRGB = 1;
    ii.oOrientation = O_NONE;
    ii.cROILeftX = ii.cROITopY = ii.cROIWidth = ii.cROIHeight = 0;
    ii.cThumbnailWidth = ii.cThumbnailHeight = 0;
    scp.uAlphaMode = alpha ? 2 : 0;
    if (ImageStrDecInit(&ii, &scp, &ctx) != ICERR_OK)
        goto done;
    memset(&bi, 0, sizeof bi);
    bi.pv = out;
    bi.cLine = height;
    bi.cbStride = stride;
    /* Re-entrant build: one macroblock row per call, output lags by one row. */
    rc = 0;
    {
        unsigned row, rows = (height + 15) / 16 + 1;
        for (row = 0; row < rows; row++) {
            size_t lines = 0;
            bi.uiFirstMBRow = row;
            bi.uiLastMBRow = row;
            if (ImageStrDecDecode(ctx, &bi, &lines) != ICERR_OK) {
                rc = -1;
                break;
            }
        }
    }
    ImageStrDecTerm(ctx);
done:
    if (ws)
        ws->Close(&ws);
    free(buf);
    return rc;
}
