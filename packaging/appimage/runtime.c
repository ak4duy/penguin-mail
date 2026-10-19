#define _GNU_SOURCE
#include <gio/gio.h>
#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* Keep runtime settings out of environ so host programs inherit their own
 * paths. Only processes started with this adapter see these values. */
static char *(*system_getenv)(const char *);
static const char *root;
static char *sandbox_launcher;
static int (*system_execvp)(const char *, char *const []);
static struct { const char *name; char *value; } settings[] = {
    {"XDG_DATA_DIRS", NULL}, {"GSETTINGS_SCHEMA_DIR", NULL},
    {"GIO_MODULE_DIR", NULL}, {"GTK_EXE_PREFIX", NULL},
    {"GTK_DATA_PREFIX", NULL}, {"LIBGL_DRIVERS_PATH", NULL},
    {"__EGL_VENDOR_LIBRARY_FILENAMES", NULL}, {"FONTCONFIG_FILE", NULL},
    {"FONTCONFIG_PATH", NULL}
};

__attribute__((constructor)) static void configure(void) {
    system_getenv = dlsym(RTLD_NEXT, "getenv");
    system_execvp = dlsym(RTLD_NEXT, "execvp");
    root = system_getenv("APPDIR");
    if (!root || root[0] != '/') abort();
    sandbox_launcher = g_strconcat(root, "/usr/bin/bwrap", NULL);
    const char *data = system_getenv("XDG_DATA_DIRS");
    settings[0].value = g_strdup_printf("%s/usr/share:%s", root, data ? data : "/usr/local/share:/usr/share");
    settings[1].value = g_strconcat(root, "/usr/share/glib-2.0/schemas", NULL);
    settings[2].value = g_strconcat(root, "/usr/lib/x86_64-linux-gnu/gio/modules", NULL);
    settings[3].value = g_strconcat(root, "/usr", NULL);
    settings[4].value = g_strconcat(root, "/usr", NULL);
    settings[5].value = g_strconcat(root, "/usr/lib/x86_64-linux-gnu/dri", NULL);
    settings[6].value = g_strconcat(root, "/usr/share/glvnd/egl_vendor.d/50_mesa.json", NULL);
    settings[7].value = g_strconcat(root, "/etc/fonts/fonts.conf", NULL);
    settings[8].value = g_strconcat(root, "/etc/fonts", NULL);
}

char *getenv(const char *name) {
    if (!system_getenv) system_getenv = dlsym(RTLD_NEXT, "getenv");
    for (size_t i = 0; i < G_N_ELEMENTS(settings); ++i)
        if (settings[i].value && !strcmp(name, settings[i].name)) return settings[i].value;
    return system_getenv(name);
}

/* WebKit's distro build has absolute helper paths. Keep its launcher,
 * namespace arguments and inherited descriptors, changing only the helper
 * command to use the bundle's dynamic loader. */
GSubprocess *g_subprocess_launcher_spawnv(GSubprocessLauncher *launcher,
                                        const gchar *const *argv, GError **error) {
    typedef GSubprocess *(*Spawn)(GSubprocessLauncher *, const gchar *const *, GError **);
    Spawn spawn = dlsym(RTLD_NEXT, "g_subprocess_launcher_spawnv");
    size_t target = 0;
    if (!strcmp(argv[0], "/usr/bin/bwrap") && argv[1] && !strcmp(argv[1], "--args")
        && argv[2] && argv[3] && !strcmp(argv[3], "--")) target = 4;
    const char *prefix = "/usr/lib/x86_64-linux-gnu/webkitgtk-6.0/";
    const char *command = argv[target];
    if (!command || !g_str_has_prefix(command, prefix)) return spawn(launcher, argv, error);
    const char *name = command + strlen(prefix);
    if (strcmp(name, "WebKitWebProcess") && strcmp(name, "WebKitNetworkProcess")
        && strcmp(name, "WebKitGPUProcess")) return spawn(launcher, argv, error);
    GPtrArray *args = g_ptr_array_new_with_free_func(g_free);
    for (size_t i = 0; i < target; ++i) g_ptr_array_add(args, g_strdup(argv[i]));
    g_ptr_array_add(args, g_strconcat(root, "/lib64/ld-linux-x86-64.so.2", NULL));
    g_ptr_array_add(args, g_strdup("--library-path"));
    g_ptr_array_add(args, g_strdup_printf("%s/usr/lib/x86_64-linux-gnu:%s/lib/x86_64-linux-gnu", root, root));
    g_ptr_array_add(args, g_strdup("--preload"));
    g_ptr_array_add(args, g_strdup("$ORIGIN/../../penguin-mail/appimage-runtime.so"));
    g_ptr_array_add(args, g_strconcat(root, command, NULL));
    for (size_t i = target + 1; argv[i]; ++i) g_ptr_array_add(args, g_strdup(argv[i]));
    g_ptr_array_add(args, NULL);
    GSubprocess *child = spawn(launcher, (const gchar *const *)args->pdata, error);
    g_ptr_array_unref(args);
    return child;
}

void *dlopen(const char *filename, int flags) {
    typedef void *(*Open)(const char *, int);
    Open open = dlsym(RTLD_NEXT, "dlopen");
    if (root && filename && g_str_has_prefix(filename, "/usr/lib/x86_64-linux-gnu/webkitgtk-6.0/")) {
        char *path = g_strconcat(root, filename, NULL);
        void *result = open(path, flags);
        g_free(path);
        return result;
    }
    return open(filename, flags);
}

/* Rust's fork/exec path searches libc's real PATH. Resolve only bubblewrap
 * here, without allocating after fork. Other commands use the host PATH. */
int execvp(const char *file, char *const argv[]) {
    if (!strcmp(file, "bwrap")) return execv(sandbox_launcher, argv);
    return system_execvp(file, argv);
}
