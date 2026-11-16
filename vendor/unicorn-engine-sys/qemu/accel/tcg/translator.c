/*
 * Generic intermediate code generation.
 *
 * Copyright (C) 2016-2017 Lluís Vilanova <vilanova@ac.upc.edu>
 *
 * This work is licensed under the terms of the GNU GPL, version 2 or later.
 * See the COPYING file in the top-level directory.
 */

#include "qemu/osdep.h"
#include "cpu.h"
#include "tcg/tcg.h"
#include "tcg/tcg-op.h"
#include "exec/exec-all.h"
#include "exec/gen-icount.h"
#include "exec/translator.h"

#include <uc_priv.h>

/* Pairs with tcg_clear_temp_count.
   To be called by #TranslatorOps.{translate_insn,tb_stop} if
   (1) the target is sufficiently clean to support reporting,
   (2) as and when all temporaries are known to be consumed.
   For most targets, (2) is at the end of translate_insn.  */
void translator_loop_temp_check(DisasContextBase *db)
{
#if 0
    if (tcg_check_temp_count()) {
        qemu_log("warning: TCG temporary leaks before "
                 TARGET_FMT_lx "\n", db->pc_next);
    }
#endif
}

/*
 * ITS-EMU: inline cycle accounting (uc_its_account). The code at the start
 * of a block does what the emulator's block hook does for most blocks, and
 * skips the hook then:
 *
 *   edge = how the previous block ended: the address after it (bit 0 set if
 *          it ended with an unconditional branch) | conditional << 32
 *          | fresh << 33 | its size << 48
 *   by_branch = (uint32_t)edge != pc
 *   refill = (edge >> 32) & by_branch        (of a taken conditional branch)
 *   slow = clock + refill >= limit
 *   clock += refill + cycles + (by_branch & penalty)
 *   if (!slow) edge = this block's; else undo and call the hook
 *
 * and for the idle detector (layout.idle): the hook also runs at its loop
 * head and at a backward branch once 64 blocks ran since the head; otherwise
 * the block counts itself and sets the detector's flags.
 *
 * The clock is advanced before the conditions are known and set back on the
 * way to the hook (the edge is still the old one there): the next block's
 * clock then waits for a load, an add and a store only, not for the
 * comparisons. The hook finds the state as if this code did not exist. No
 * temporary lives across a branch.
 * Cycles, penalty and the new edge are known once the block is translated:
 * its_finish_account patches them in, or removes all of it.
 * The PC is written (pc_sync) for the hook only: blocks that charge
 * themselves leave it as QEMU does, to whoever stops the block.
 */
typedef struct ItsAccount {
    TCGOp *before;             /* last op before the accounting code */
    TCGOp *last;               /* its last op (the hook call follows) */
    TCGOp *cycles[2], *penalty[2], *post; /* movi_i64 to patch */
    TCGOp *stores[2], *side[2]; /* op before, last op of the flag updates */
    TCGLabel *done;
} ItsAccount;

static TCGv_i64 its_patched_const(TCGContext *s, TCGOp **op)
{
    TCGv_i64 t = tcg_const_i64(s, 1);
    *op = tcg_last_op(s);
    assert((*op)->opc == INDEX_op_movi_i64);
    return t;
}

static void its_gen_account(TCGContext *s, struct uc_struct *uc, uint64_t pc,
                            ItsAccount *g)
{
    const struct uc_its_layout *a = &uc->its_acct;
    TCGLabel *slow_path = gen_new_label(s);
    TCGv_ptr base;
    TCGv_i64 addr, edge, low, by_branch, slow, clock, t, d, k, n = NULL;

    g->before = tcg_last_op(s);
    g->done = gen_new_label(s);
    base = tcg_const_ptr(s, a->base);
    addr = tcg_const_i64(s, pc);
    edge = tcg_temp_new_i64(s);
    low = tcg_temp_new_i64(s);
    by_branch = tcg_temp_new_i64(s);
    slow = tcg_temp_new_i64(s);
    clock = tcg_temp_new_i64(s);
    t = tcg_temp_new_i64(s);
    d = tcg_temp_new_i64(s);

    tcg_gen_ld_i64(s, edge, base, a->edge);
    tcg_gen_ext32u_i64(s, low, edge);
    tcg_gen_setcond_i64(s, TCG_COND_NE, by_branch, low, addr);
    tcg_gen_ld_i64(s, clock, base, a->clock);
    tcg_gen_shri_i64(s, d, edge, 32);
    tcg_gen_and_i64(s, d, d, by_branch);
    tcg_gen_add_i64(s, t, clock, d);
    tcg_gen_ld_i64(s, slow, base, a->limit);
    tcg_gen_setcond_i64(s, TCG_COND_GEU, slow, t, slow);

    if (a->idle) {
        TCGv_i64 u = tcg_temp_new_i64(s);

        n = tcg_temp_new_i64(s);
        /* backward (pc <= previous block's address) after 64 blocks */
        tcg_gen_shri_i64(s, t, edge, 48);
        tcg_gen_sub_i64(s, t, low, t);
        tcg_gen_setcond_i64(s, TCG_COND_GEU, t, t, addr);
        tcg_gen_ld32u_i64(s, n, base, a->idle_blocks);
        tcg_gen_setcondi_i64(s, TCG_COND_GEU, u, n, 64);
        tcg_gen_and_i64(s, t, t, u);
        tcg_gen_or_i64(s, slow, slow, t);
        /* the loop head */
        tcg_gen_ld_i64(s, t, base, a->idle_head);
        tcg_gen_setcond_i64(s, TCG_COND_EQ, t, t, addr);
        tcg_gen_or_i64(s, slow, slow, t);
        tcg_gen_addi_i64(s, n, n, 1);
        tcg_gen_st32_i64(s, n, base, a->idle_blocks);
        tcg_temp_free_i64(s, u);
        tcg_temp_free_i64(s, n);
    }

    k = its_patched_const(s, &g->penalty[0]);
    tcg_gen_and_i64(s, t, by_branch, k);
    tcg_gen_add_i64(s, d, d, t);
    tcg_temp_free_i64(s, k);
    k = its_patched_const(s, &g->cycles[0]);
    tcg_gen_add_i64(s, d, d, k);
    tcg_temp_free_i64(s, k);
    tcg_gen_add_i64(s, clock, clock, d);
    tcg_gen_st_i64(s, clock, base, a->clock);
    tcg_gen_brcondi_i64(s, TCG_COND_NE, slow, 0, slow_path);

    tcg_temp_free_i64(s, d);
    tcg_temp_free_i64(s, t);
    tcg_temp_free_i64(s, clock);
    tcg_temp_free_i64(s, slow);
    tcg_temp_free_i64(s, by_branch);
    tcg_temp_free_i64(s, low);
    tcg_temp_free_i64(s, edge);
    tcg_temp_free_i64(s, addr);
    tcg_temp_free_ptr(s, base);

    /* the block is charged: its edge and the idle detector's flags */
    base = tcg_const_ptr(s, a->base);
    k = its_patched_const(s, &g->post);
    tcg_gen_st_i64(s, k, base, a->edge);
    tcg_temp_free_i64(s, k);
    if (a->idle) {
        g->stores[0] = tcg_last_op(s);
        k = tcg_const_i64(s, 1);
        tcg_gen_st8_i64(s, k, base, a->idle_stored);
        tcg_temp_free_i64(s, k);
        g->stores[1] = tcg_last_op(s);
        g->side[0] = tcg_last_op(s);
        k = tcg_const_i64(s, 0);
        tcg_gen_st8_i64(s, k, base, a->idle_clean);
        tcg_gen_st8_i64(s, k, base, a->idle_delay_clean);
        tcg_temp_free_i64(s, k);
        g->side[1] = tcg_last_op(s);
    }
    tcg_temp_free_ptr(s, base);
    tcg_gen_br(s, g->done);

    /* not charged: as before, then the hook */
    gen_set_label(s, slow_path);
    base = tcg_const_ptr(s, a->base);
    addr = tcg_const_i64(s, pc);
    clock = tcg_temp_new_i64(s);
    d = tcg_temp_new_i64(s);
    t = tcg_temp_new_i64(s);
    /* what was added above, from the same values */
    tcg_gen_ld_i64(s, d, base, a->edge);
    tcg_gen_ext32u_i64(s, t, d);
    tcg_gen_setcond_i64(s, TCG_COND_NE, t, t, addr);
    tcg_gen_shri_i64(s, d, d, 32);
    tcg_gen_and_i64(s, d, d, t);
    k = its_patched_const(s, &g->penalty[1]);
    tcg_gen_and_i64(s, t, t, k);
    tcg_gen_add_i64(s, d, d, t);
    tcg_temp_free_i64(s, k);
    k = its_patched_const(s, &g->cycles[1]);
    tcg_gen_add_i64(s, d, d, k);
    tcg_temp_free_i64(s, k);
    tcg_gen_ld_i64(s, clock, base, a->clock);
    tcg_gen_sub_i64(s, clock, clock, d);
    tcg_gen_st_i64(s, clock, base, a->clock);
    tcg_temp_free_i64(s, t);
    tcg_temp_free_i64(s, addr);
    if (a->idle) {
        tcg_gen_ld32u_i64(s, d, base, a->idle_blocks);
        tcg_gen_subi_i64(s, d, d, 1);
        tcg_gen_st32_i64(s, d, base, a->idle_blocks);
    }
    tcg_temp_free_i64(s, d);
    tcg_temp_free_i64(s, clock);
    tcg_temp_free_ptr(s, base);
    g->last = tcg_last_op(s);
}

/* Removes the ops after `before` up to `last`. */
static void its_remove_ops(TCGContext *s, TCGOp *before, TCGOp *last)
{
    TCGOp *op = before ? QTAILQ_NEXT(before, link) : QTAILQ_FIRST(&s->ops);

    for (;;) {
        TCGOp *next = QTAILQ_NEXT(op, link);
        bool end = op == last;
        tcg_op_remove(s, op);
        if (end) {
            break;
        }
        op = next;
    }
}

static void its_finish_account(TCGContext *s, struct uc_struct *uc,
                               TranslationBlock *tb, ItsAccount *g)
{
    struct uc_its_block b;

    memset(&b, 0, sizeof(b));
    if (!uc->its_block(uc->its_block_opaque, tb->pc, tb->size, &b)) {
        /* always the hook (the label after it stays, unused) */
        its_remove_ops(s, g->before, g->last);
        return;
    }
    g->cycles[0]->args[1] = g->cycles[1]->args[1] = b.cycles;
    g->penalty[0]->args[1] = g->penalty[1]->args[1] = b.penalty;
    g->post->args[1] = b.post;
    if (uc->its_acct.idle) {
        /* in this order: the second range starts at the first one's end */
        if (!b.side_effects) {
            its_remove_ops(s, g->side[0], g->side[1]);
        }
        if (!b.stores) {
            its_remove_ops(s, g->stores[0], g->stores[1]);
        }
    }
}

void translator_loop(const TranslatorOps *ops, DisasContextBase *db,
                     CPUState *cpu, TranslationBlock *tb, int max_insns)
{
    int bp_insn = 0;
    struct uc_struct *uc = (struct uc_struct *)cpu->uc;
    TCGContext *tcg_ctx = uc->tcg_ctx;
    TCGOp *prev_op = NULL;
    bool block_hook = false;
    ItsAccount its_account;
    bool its = false;

    /* Initialize DisasContext */
    db->tb = tb;
    db->pc_first = tb->pc;
    db->pc_next = db->pc_first;
    db->is_jmp = DISAS_NEXT;
    db->num_insns = 0;
    db->max_insns = max_insns;
    db->singlestep_enabled = cpu->singlestep_enabled;

    ops->init_disas_context(db, cpu);
    tcg_debug_assert(db->is_jmp == DISAS_NEXT);  /* no early exit */

    /* Reset the temp count so that we can identify leaks */
    tcg_clear_temp_count();

    /* Unicorn: early check to see if the address of this block is
     * the "run until" address. */
    if (uc_addr_is_exit(uc, tb->pc)) {
        // This should catch that instruction is at the end
        // and generate appropriate halting code.
        gen_tb_start(tcg_ctx, db->tb);
        ops->tb_start(db, cpu);
        db->num_insns++;
        ops->insn_start(db, cpu);
        ops->translate_insn(db, cpu);
        goto _end_loop;
    }

    /* Unicorn: trace this block on request
     * Only hook this block if it is not broken from previous translation due to
     * full translation cache
     */
    if (HOOK_EXISTS_BOUNDED(uc, UC_HOOK_BLOCK, tb->pc)) {
        if (uc->its_block) {
            /* ITS-EMU: the hook only if the block cannot account for itself */
            its = true;
            its_gen_account(tcg_ctx, uc, db->pc_first, &its_account);
        }
        ops->pc_sync(db, cpu);
        prev_op = tcg_last_op(tcg_ctx);
        block_hook = true;
        gen_uc_tracecode(tcg_ctx, 0xf8f8f8f8, UC_HOOK_BLOCK_IDX, uc, db->pc_first);
        if (its) {
            gen_set_label(tcg_ctx, its_account.done);
        }
    }

    // tcg_dump_ops(tcg_ctx, false, "translator loop");

    /* Start translating.  */
    gen_tb_start(tcg_ctx, db->tb);
    // tcg_dump_ops(tcg_ctx, false, "tb start");

    ops->tb_start(db, cpu);
    // tcg_dump_ops(tcg_ctx, false, "tb start 2");

    tcg_debug_assert(db->is_jmp == DISAS_NEXT);  /* no early exit */

    while (true) {
        db->num_insns++;

        ops->insn_start(db, cpu);
        tcg_debug_assert(db->is_jmp == DISAS_NEXT);  /* no early exit */

        /* Pass breakpoint hits to target for further processing */
        if (!db->singlestep_enabled
            && unlikely(!QTAILQ_EMPTY(&cpu->breakpoints))) {
            CPUBreakpoint *bp;
            QTAILQ_FOREACH(bp, &cpu->breakpoints, entry) {
                if (bp->pc == db->pc_next) {
                    if (ops->breakpoint_check(db, cpu, bp)) {
                        bp_insn = 1;
                        break;
                    }
                }
            }
            /* The breakpoint_check hook may use DISAS_TOO_MANY to indicate
               that only one more instruction is to be executed.  Otherwise
               it should use DISAS_NORETURN when generating an exception,
               but may use a DISAS_TARGET_* value for Something Else.  */
            if (db->is_jmp > DISAS_TOO_MANY) {
                break;
            }
        }

        /* Disassemble one instruction.  The translate_insn hook should
           update db->pc_next and db->is_jmp to indicate what should be
           done next -- either exiting this loop or locate the start of
           the next instruction.  */
        ops->translate_insn(db, cpu);
        // tcg_dump_ops(tcg_ctx, false, "insn translate");

        /* Stop translation if translate_insn so indicated.  */
        if (db->is_jmp != DISAS_NEXT) {
            break;
        }

        /* Stop translation if the output buffer is full,
           or we have executed all of the allowed instructions.  */
        if (tcg_op_buf_full(tcg_ctx) || db->num_insns >= db->max_insns) {
            db->is_jmp = DISAS_TOO_MANY;
            break;
        }
    }

_end_loop:
    /* Emit code to exit the TB, as indicated by db->is_jmp.  */
    ops->tb_stop(db, cpu);
    gen_tb_end(tcg_ctx, db->tb, db->num_insns - bp_insn);
    // tcg_dump_ops(tcg_ctx, false, "tb end");

    /* The disas_log hook may use these values rather than recompute.  */
    db->tb->size = db->pc_next - db->pc_first;
    db->tb->icount = db->num_insns;

    hooked_regions_check(uc, db->tb->pc, db->tb->size);

    if (block_hook) {
        TCGOp *tcg_op;

        // Unicorn: patch the callback to have the proper block size.
        if (prev_op) {
            // As explained further up in the function where prev_op is
            // assigned, we move forward in the tail queue, so we're modifying the
            // move instruction generated by gen_uc_tracecode() that contains
            // the instruction size to assign the proper size (replacing 0xF1F1F1F1).
            tcg_op = QTAILQ_NEXT(prev_op, link);
        } else {
            // this basic block is the first emulated code ever,
            // so the basic block operand is the first operand
            tcg_op = QTAILQ_FIRST(&tcg_ctx->ops);
        }

        tcg_op->args[1] = db->tb->size;
    }

    if (its) {
        its_finish_account(tcg_ctx, uc, db->tb, &its_account);
    }
}
