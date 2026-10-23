/* CRC-32 for libmpcdec, replacing common/crc32.c from the Musepack sources,
 * which fills a shared table on first use from any thread. Only the SV8
 * stream header needs it, once per opened file. Written for nyanoto. */

unsigned long mpc_crc32(unsigned char *buf, int len)
{
    unsigned long crc = 0xffffffffUL;
    int n, k;

    for (n = 0; n < len; n++) {
        crc ^= buf[n];
        for (k = 0; k < 8; k++)
            crc = (crc & 1) ? 0xedb88320UL ^ (crc >> 1) : crc >> 1;
    }
    return crc ^ 0xffffffffUL;
}
