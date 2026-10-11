#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <setjmp.h>
#include <ctype.h>

static jmp_buf jb;
static int cmp_int(const void *a, const void *b) { return *(const int *)a - *(const int *)b; }
static void bye(void) { printf("atexit handler ran\n"); }

int main(int argc, char **argv) {
    atexit(bye);
    printf("argc=%d argv0_has_exe=%d\n", argc, strstr(argv[0], ".exe") != NULL);
    for (int i = 1; i < argc; i++) printf("arg%d=%s\n", i, argv[i]);

    int a[8] = {42, 7, 19, 3, 88, 1, 56, 23};
    qsort(a, 8, sizeof a[0], cmp_int);
    for (int i = 0; i < 8; i++) printf("%d%c", a[i], i == 7 ? '\n' : ' ');

    char buf[64];
    snprintf(buf, sizeof buf, "%5.2f|%-6s|%04d|%x|%lld", 3.14159, "ab", 42, 255, 1234567890123LL);
    puts(buf);

    char *p = malloc(100);
    strcpy(p, "heap string");
    strcat(p, "!");
    printf("%s len=%zu\n", p, strlen(p));
    p = realloc(p, 5000);
    printf("realloc kept: %s\n", p);
    free(p);

    printf("sqrt=%.4f pow=%.1f sin=%.4f floor=%.0f\n", sqrt(2.0), pow(2.0, 10.0), sin(1.0), floor(-2.5));

    int r = setjmp(jb);
    if (r == 0) { printf("before longjmp\n"); longjmp(jb, 7); }
    printf("after longjmp r=%d\n", r);

    FILE *f = fopen("out.txt", "w");
    fprintf(f, "line %d\nline %d\n", 1, 2);
    fclose(f);
    f = fopen("out.txt", "r");
    char line[32]; int n = 0;
    while (fgets(line, sizeof line, f)) { n++; printf("read: %s", line); }
    fclose(f);
    printf("lines=%d toupper=%c isdigit=%d\n", n, toupper('q'), isdigit('7') != 0);

    int x, y; char word[16];
    int got = sscanf("12 34 hello", "%d %d %s", &x, &y, word);
    printf("sscanf=%d x+y=%d word=%s\n", got, x + y, word);
    fprintf(stderr, "to stderr\n");
    return 7;
}
