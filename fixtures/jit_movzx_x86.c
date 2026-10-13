// Small PE32 fixture for the adjacent MOVZX instructions observed in GTA's parser.
static volatile unsigned char source = 0xa5;

__declspec(naked) int main(void) {
    __asm {
        mov esi, offset source
        mov al, byte ptr [esi]
        jmp scan_character
    scan_character:
        movzx ebx, byte ptr [esi]
        movzx eax, bl
        cmp al, 0xa5
        jne failed
        xor eax, eax
        ret
    failed:
        mov eax, 1
        ret
    }
}
