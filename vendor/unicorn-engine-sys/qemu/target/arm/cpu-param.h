/*
 * ARM cpu parameters for qemu.
 *
 * Copyright (c) 2003 Fabrice Bellard
 * SPDX-License-Identifier: LGPL-2.0+
 */

#ifndef ARM_CPU_PARAM_H
#define ARM_CPU_PARAM_H 1

#ifdef TARGET_AARCH64
# define TARGET_LONG_BITS             64
# define TARGET_PHYS_ADDR_SPACE_BITS  48
# define TARGET_VIRT_ADDR_SPACE_BITS  48
#else
# define TARGET_LONG_BITS             32
# define TARGET_PHYS_ADDR_SPACE_BITS  40
# define TARGET_VIRT_ADDR_SPACE_BITS  32
#endif

/*
 * ARMv7 and later CPUs have 4K pages minimum, but ARMv5 and v6
 * have to support 1K tiny pages.
 */
# define TARGET_PAGE_BITS_VARY
# define TARGET_PAGE_BITS_MIN  10
/*
 * ITS-EMU: the page size is never changed (UC_CTL_UC_PAGE_SIZE is not used):
 * a constant saves loading it through uc->init_target_page in every TB
 * lookup, TLB index and page mask.
 */
# define TARGET_PAGE_BITS_FIXED 10

#define NB_MMU_MODES 12

#endif
