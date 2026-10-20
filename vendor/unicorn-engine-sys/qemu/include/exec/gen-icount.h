#ifndef GEN_ICOUNT_H
#define GEN_ICOUNT_H

#include "qemu/timer.h"

/* Helpers for instruction counting code generation.  */

static inline void gen_io_start(TCGContext *tcg_ctx)
{
    TCGv_i32 tmp = tcg_const_i32(tcg_ctx, 1);
    tcg_gen_st_i32(tcg_ctx, tmp, tcg_ctx->cpu_env,
                   offsetof(ArchCPU, parent_obj.can_do_io) -
                   offsetof(ArchCPU, env));
    tcg_temp_free_i32(tcg_ctx, tmp);
}

/*
 * cpu->can_do_io is cleared automatically at the beginning of
 * each translation block.  The cost is minimal and only paid
 * for -icount, plus it would be very easy to forget doing it
 * in the translator.  Therefore, backends only need to call
 * gen_io_start.
 */
static inline void gen_io_end(TCGContext *tcg_ctx)
{
    TCGv_i32 tmp = tcg_const_i32(tcg_ctx, 0);
    tcg_gen_st_i32(tcg_ctx, tmp, tcg_ctx->cpu_env,
                   offsetof(ArchCPU, parent_obj.can_do_io) -
                   offsetof(ArchCPU, env));
    tcg_temp_free_i32(tcg_ctx, tmp);
}

static inline void gen_tb_start(TCGContext *tcg_ctx, TranslationBlock *tb)
{
    // ITS-EMU: without a delay slot, test the exit request inline as QEMU
    // does instead of calling a helper (which syncs all globals) per TB.
    // An exit leaves before the first instruction with TB_EXIT_REQUESTED;
    // cpu_tb_exec then sets the PC to the TB's start.
    if (tcg_ctx->delay_slot_flag == NULL) {
        TCGv_i32 count = tcg_temp_new_i32(tcg_ctx);
        tcg_gen_ld_i32(tcg_ctx, count, tcg_ctx->cpu_env,
                       offsetof(ArchCPU, neg.icount_decr.u32) -
                       offsetof(ArchCPU, env));
        tcg_ctx->exitreq_label = gen_new_label(tcg_ctx);
        tcg_gen_brcondi_i32(tcg_ctx, TCG_COND_LT, count, 0, tcg_ctx->exitreq_label);
        tcg_temp_free_i32(tcg_ctx, count);
        return;
    }
    tcg_ctx->exitreq_label = NULL;

    TCGv_ptr puc = tcg_const_ptr(tcg_ctx, tcg_ctx->uc);
    TCGv_i32 tmp = tcg_const_i32(tcg_ctx, 0);
    // Unicorn:
    //    We CANT'T use brcondi_i32 here or we will fail liveness analysis
    //    because it marks the end of BB
    if (tcg_ctx->delay_slot_flag != NULL) {
        tcg_gen_mov_i32(tcg_ctx, tmp, tcg_ctx->delay_slot_flag);
    }
    gen_helper_check_exit_request(tcg_ctx, puc, tmp);
    tcg_temp_free_i32(tcg_ctx, tmp);
    tcg_temp_free_ptr(tcg_ctx, puc);
}

static inline void gen_tb_end(TCGContext *tcg_ctx, TranslationBlock *tb, int num_insns)
{
    if (tcg_ctx->delay_slot_flag != NULL){
        tcg_temp_free_i32(tcg_ctx, tcg_ctx->delay_slot_flag);
    }
    tcg_ctx->delay_slot_flag = NULL;
    if (tb_cflags(tb) & CF_USE_ICOUNT) {
        /* Update the num_insn immediate parameter now that we know
         * the actual insn count.  */
        tcg_set_insn_param(tcg_ctx->icount_start_insn, 1, num_insns);
    }

    if (tcg_ctx->exitreq_label) {
        gen_set_label(tcg_ctx, tcg_ctx->exitreq_label);
        tcg_ctx->exitreq_label = NULL;
    }
    tcg_gen_exit_tb(tcg_ctx, tb, TB_EXIT_REQUESTED);
}

#endif
