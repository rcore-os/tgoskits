/* Generate fixtures with a host libjpeg development installation:
 * cc generate.c -ljpeg -o generate
 * ./generate yuv420.jpg 2 2; ./generate yuv422h.jpg 2 1
 * ./generate yuv422v.jpg 1 2; ./generate yuv444.jpg 1 1
 */
#include <stdio.h>
#include <stdlib.h>
#include <jpeglib.h>
int main(int argc, char **argv) {
    if (argc != 4) return 1;
    FILE *f = fopen(argv[1], "wb");
    if (!f) return 1;
    struct jpeg_compress_struct c;
    struct jpeg_error_mgr error;
    c.err = jpeg_std_error(&error);
    jpeg_create_compress(&c);
    jpeg_stdio_dest(&c, f);
    c.image_width = c.image_height = 32;
    c.input_components = 3;
    c.in_color_space = JCS_YCbCr;
    jpeg_set_defaults(&c);
    c.comp_info[0].h_samp_factor = atoi(argv[2]);
    c.comp_info[0].v_samp_factor = atoi(argv[3]);
    jpeg_set_quality(&c, 100, TRUE);
    jpeg_start_compress(&c, TRUE);
    while (c.next_scanline < c.image_height) {
        unsigned char row[32 * 3];
        for (int x = 0; x < 32; ++x) {
            row[3*x] = 96 + 32 * (x / 16);
            row[3*x+1] = 64 + 64 * (x / 16);
            row[3*x+2] = 96 + 64 * (c.next_scanline / 16);
        }
        JSAMPROW rows[] = {row};
        jpeg_write_scanlines(&c, rows, 1);
    }
    jpeg_finish_compress(&c);
    jpeg_destroy_compress(&c);
    return fclose(f) != 0;
}
