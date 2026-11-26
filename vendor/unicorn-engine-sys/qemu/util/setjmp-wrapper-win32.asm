PUBLIC _setjmp_wrapper
PUBLIC _longjmp_wrapper

_TEXT SEGMENT

; Non-unwinding setjmp/longjmp for Win64 MSVC.
;
; Unicorn's original wrapper called UCRT's _setjmp(env, NULL) so that longjmp
; would skip SEH unwinding (which cannot walk TCG JIT frames). On Windows on
; Arm under x64 emulation, UCRT's longjmp still invokes RtlUnwindEx even with
; a NULL frame, walking past the caller into Rust's nounwind thread_start and
; aborting with STATUS_STACK_BUFFER_OVERRUN.
;
; Saving and restoring the Win64 callee-saved registers directly avoids UCRT's
; unwinder on both native x64 and Windows on Arm. The layout fits inside
; MSVC's 256-byte jmp_buf and uses unaligned XMM moves.

_setjmp_wrapper PROC
    mov QWORD PTR [rcx + 00h], rbx
    mov QWORD PTR [rcx + 08h], rbp
    mov QWORD PTR [rcx + 10h], rdi
    mov QWORD PTR [rcx + 18h], rsi
    mov QWORD PTR [rcx + 20h], r12
    mov QWORD PTR [rcx + 28h], r13
    mov QWORD PTR [rcx + 30h], r14
    mov QWORD PTR [rcx + 38h], r15
    lea rax, [rsp + 8]
    mov QWORD PTR [rcx + 40h], rax
    mov rax, QWORD PTR [rsp]
    mov QWORD PTR [rcx + 48h], rax
    stmxcsr DWORD PTR [rcx + 50h]
    fnstcw WORD PTR [rcx + 54h]
    movups XMMWORD PTR [rcx + 60h], xmm6
    movups XMMWORD PTR [rcx + 70h], xmm7
    movups XMMWORD PTR [rcx + 80h], xmm8
    movups XMMWORD PTR [rcx + 90h], xmm9
    movups XMMWORD PTR [rcx + 0A0h], xmm10
    movups XMMWORD PTR [rcx + 0B0h], xmm11
    movups XMMWORD PTR [rcx + 0C0h], xmm12
    movups XMMWORD PTR [rcx + 0D0h], xmm13
    movups XMMWORD PTR [rcx + 0E0h], xmm14
    movups XMMWORD PTR [rcx + 0F0h], xmm15
    xor eax, eax
    ret
_setjmp_wrapper ENDP

_longjmp_wrapper PROC
    mov rbx, QWORD PTR [rcx + 00h]
    mov rbp, QWORD PTR [rcx + 08h]
    mov rdi, QWORD PTR [rcx + 10h]
    mov rsi, QWORD PTR [rcx + 18h]
    mov r12, QWORD PTR [rcx + 20h]
    mov r13, QWORD PTR [rcx + 28h]
    mov r14, QWORD PTR [rcx + 30h]
    mov r15, QWORD PTR [rcx + 38h]
    ldmxcsr DWORD PTR [rcx + 50h]
    fldcw WORD PTR [rcx + 54h]
    movups xmm6, XMMWORD PTR [rcx + 60h]
    movups xmm7, XMMWORD PTR [rcx + 70h]
    movups xmm8, XMMWORD PTR [rcx + 80h]
    movups xmm9, XMMWORD PTR [rcx + 90h]
    movups xmm10, XMMWORD PTR [rcx + 0A0h]
    movups xmm11, XMMWORD PTR [rcx + 0B0h]
    movups xmm12, XMMWORD PTR [rcx + 0C0h]
    movups xmm13, XMMWORD PTR [rcx + 0D0h]
    movups xmm14, XMMWORD PTR [rcx + 0E0h]
    movups xmm15, XMMWORD PTR [rcx + 0F0h]
    mov eax, edx
    test eax, eax
    jnz val_ok
    inc eax
val_ok:
    mov rdx, QWORD PTR [rcx + 48h]
    mov rsp, QWORD PTR [rcx + 40h]
    jmp rdx
_longjmp_wrapper ENDP

_TEXT ENDS

END