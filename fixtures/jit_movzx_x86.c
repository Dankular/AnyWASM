// Small PE32 fixture for MOVZX and scaled LEA instructions observed in GTA's parser.
static volatile unsigned char source = 0xa5;

__declspec(naked) int main(void) {
    __asm {
        mov esi, offset source
        mov al, byte ptr [esi]
        mov edi, 2
        jmp scan_character
    scan_character:
        movzx ebx, byte ptr [esi]
        movzx eax, bl
        lea eax, [edi + edi*4]
        lea edi, [ebx + eax*2 - 0x30]
        cmp eax, 10
        jne failed
        cmp edi, 0x89
        jne failed
        xor eax, eax
        ret
    failed:
        mov eax, 1
        ret
    }
}
