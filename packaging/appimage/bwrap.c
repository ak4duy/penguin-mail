#define _GNU_SOURCE
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Glycin clears the helper's environment and mounts only host library
 * directories. Preserve its restrictions and add this bundle read-only. */
int main(int argc, char **argv) {
    char root[PATH_MAX];
    ssize_t len = readlink("/proc/self/exe", root, sizeof(root) - 1);
    const char suffix[] = "/usr/bin/bwrap";
    if (len < (ssize_t)sizeof(suffix)) return 125;
    root[len] = 0;
    if (strcmp(root + len - strlen(suffix), suffix)) return 125;
    root[len - strlen(suffix)] = 0;
    size_t target = 0;
    for (int i = 3; i + 2 < argc; ++i) {
        if (!strcmp(argv[i - 2], "--seccomp")
            && !strncmp(argv[i], "/usr/libexec/glycin-loaders/2+/glycin-", strlen("/usr/libexec/glycin-loaders/2+/glycin-"))
            && !strcmp(argv[i + 1], "--dbus-fd")) target = (size_t)i;
    }
    if (!target) { execv("/usr/bin/bwrap", argv); perror("bwrap"); return 125; }
    char loader[PATH_MAX], libraries[PATH_MAX * 2], helper[PATH_MAX];
    if (snprintf(loader, sizeof(loader), "%s/lib64/ld-linux-x86-64.so.2", root) >= (int)sizeof(loader)
        || snprintf(libraries, sizeof(libraries), "%s/usr/lib/x86_64-linux-gnu:%s/lib/x86_64-linux-gnu", root, root) >= (int)sizeof(libraries)
        || snprintf(helper, sizeof(helper), "%s%s", root, argv[target]) >= (int)sizeof(helper)) return 125;
    char **args = calloc((size_t)argc + 14, sizeof(char *));
    if (!args) return 125;
    size_t n = 0;
    args[n++] = "/usr/bin/bwrap";
    for (size_t i = 1; i < target; ++i) args[n++] = argv[i];
    /* After glycin's own mounts, so its /usr cannot cover a bundle there. */
    args[n++] = "--ro-bind"; args[n++] = root; args[n++] = root;
    char fonts[PATH_MAX];
    if (snprintf(fonts, sizeof(fonts), "%s/etc/fonts/fonts.conf", root) >= (int)sizeof(fonts)) return 125;
    args[n++] = "--setenv"; args[n++] = "FONTCONFIG_FILE"; args[n++] = fonts;
    args[n++] = loader; args[n++] = "--library-path"; args[n++] = libraries;
    args[n++] = helper;
    for (size_t i = target + 1; i < (size_t)argc; ++i) args[n++] = argv[i];
    execv(args[0], args);
    perror("bwrap");
    return 125;
}
