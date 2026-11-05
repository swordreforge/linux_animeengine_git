// Thin C wrapper over the vendored bc7enc_rdo sources: whole-image BC7 encode
// and decode with C linkage, so the Rust side never has to model the C++ types.
#include <stdint.h>
#include <string.h>

#include "bc7enc.h"
#include "bc7decomp.h"

extern "C" {

void anima_bc7_init(void)
{
	bc7enc_compress_block_init();
}

// Returns the bytes written (blocks * 16), or 0 when `out_size` is too small.
uint64_t anima_bc7_encode(const uint8_t *pixels, uint32_t width, uint32_t height, uint32_t uber_level,
	int perceptual, uint8_t *out, uint64_t out_size)
{
	const uint32_t blocks_x = (width + 3) / 4;
	const uint32_t blocks_y = (height + 3) / 4;
	const uint64_t needed = (uint64_t)blocks_x * blocks_y * 16;
	if (out_size < needed)
		return 0;

	bc7enc_compress_block_params params;
	bc7enc_compress_block_params_init(&params);
	if (perceptual)
		bc7enc_compress_block_params_init_perceptual_weights(&params);
	else
		bc7enc_compress_block_params_init_linear_weights(&params);
	params.m_uber_level = uber_level > BC7ENC_MAX_UBER_LEVEL ? BC7ENC_MAX_UBER_LEVEL : uber_level;

	uint8_t block_pixels[64];
	for (uint32_t by = 0; by < blocks_y; ++by)
	{
		for (uint32_t bx = 0; bx < blocks_x; ++bx)
		{
			for (uint32_t y = 0; y < 4; ++y)
			{
				uint32_t sy = by * 4 + y;
				if (sy >= height)
					sy = height - 1;
				for (uint32_t x = 0; x < 4; ++x)
				{
					uint32_t sx = bx * 4 + x;
					if (sx >= width)
						sx = width - 1;
					memcpy(&block_pixels[(y * 4 + x) * 4], &pixels[((uint64_t)sy * width + sx) * 4], 4);
				}
			}
			bc7enc_compress_block(&out[((uint64_t)by * blocks_x + bx) * 16], block_pixels, &params);
		}
	}
	return needed;
}

// Decodes `blocks` back into straight RGBA8 (used to measure quality).
void anima_bc7_decode(const uint8_t *blocks, uint32_t width, uint32_t height, uint8_t *out)
{
	const uint32_t blocks_x = (width + 3) / 4;
	const uint32_t blocks_y = (height + 3) / 4;
	bc7decomp::color_rgba pixels[16];
	for (uint32_t by = 0; by < blocks_y; ++by)
	{
		for (uint32_t bx = 0; bx < blocks_x; ++bx)
		{
			if (!bc7decomp::unpack_bc7(&blocks[((uint64_t)by * blocks_x + bx) * 16], pixels))
				memset(pixels, 0, sizeof(pixels));
			for (uint32_t y = 0; y < 4; ++y)
			{
				uint32_t sy = by * 4 + y;
				if (sy >= height)
					continue;
				for (uint32_t x = 0; x < 4; ++x)
				{
					uint32_t sx = bx * 4 + x;
					if (sx >= width)
						continue;
					memcpy(&out[((uint64_t)sy * width + sx) * 4], pixels[y * 4 + x].m_comps, 4);
				}
			}
		}
	}
}

} // extern "C"
