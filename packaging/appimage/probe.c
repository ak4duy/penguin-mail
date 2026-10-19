#include <gtk/gtk.h>
#include <webkit/webkit.h>
#include <stdio.h>
#include <libsoup/soup.h>
static GMainLoop *loop;
static int result = 1;
static void evaluated(GObject *source, GAsyncResult *res, gpointer data) {
    GError *error = NULL;
    JSCValue *value = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(source), res, &error);
    if (error) fprintf(stderr, "JavaScript: %s\n", error->message);
    else { result = jsc_value_to_boolean(value) ? 0 : 2; printf("HTML and inline image: %s\n", result == 0 ? "PASS" : "FAIL"); }
    g_main_loop_quit(loop);
}
static void loaded(WebKitWebView *view, WebKitLoadEvent event, gpointer data) {
    if (event == WEBKIT_LOAD_FINISHED)
        webkit_web_view_evaluate_javascript(view, "document.getElementById('image').naturalWidth === 1 && document.body.innerText.includes('runtime probe')", -1, NULL, NULL, NULL, evaluated, NULL);
}
static gboolean expired(gpointer data) { fprintf(stderr, "Rendering timed out\n"); g_main_loop_quit(loop); return G_SOURCE_REMOVE; }
int main(int argc, char **argv) {
    gtk_init();
    GError *host_error = NULL;
    GSubprocess *host = g_subprocess_new(G_SUBPROCESS_FLAGS_STDOUT_PIPE, &host_error, "/usr/bin/env", NULL);
    char *output = NULL;
    if (!host || !g_subprocess_communicate_utf8(host, NULL, NULL, &output, NULL, &host_error)) return 4;
    const char *private_keys[] = {"LD_LIBRARY_PATH=", "LD_PRELOAD=", "GIO_MODULE_DIR=", "LIBGL_DRIVERS_PATH=", "__EGL_VENDOR_LIBRARY_FILENAMES="};
    for (unsigned i = 0; i < G_N_ELEMENTS(private_keys); ++i)
        if (strstr(output, private_keys[i])) { fprintf(stderr, "Host environment contains %s\n", private_keys[i]); return 4; }
    g_free(output); g_object_unref(host);
    puts("Host environment: PASS");
    if (argc > 3) {
        host = g_subprocess_new(G_SUBPROCESS_FLAGS_STDOUT_SILENCE, &host_error, "/usr/bin/gpg", "--version", NULL);
        if (!host || !g_subprocess_wait_check(host, NULL, &host_error)) return 6;
        g_object_unref(host);
        puts("Host GnuPG: PASS");
    }
    if (argc > 2) {
        SoupSession *session = g_object_new(SOUP_TYPE_SESSION, "timeout", 20, NULL);
        SoupMessage *message = soup_message_new("GET", argv[2]);
        GBytes *body = soup_session_send_and_read(session, message, NULL, &host_error);
        if (!body || soup_message_get_status(message) != 200) {
            fprintf(stderr, "HTTPS failed: %s\n", host_error ? host_error->message : "unexpected status"); return 5;
        }
        puts("HTTPS certificate verification: PASS");
        g_bytes_unref(body); g_object_unref(message); g_object_unref(session);
    }
    if (argc > 1) {
        GError *error = NULL;
        GdkTexture *texture = gdk_texture_new_from_filename(argv[1], &error);
        if (!texture) { fprintf(stderr, "Sandboxed SVG: %s\n", error->message); return 3; }
        printf("Sandboxed SVG: PASS (%d x %d)\n", gdk_texture_get_width(texture), gdk_texture_get_height(texture));
        g_object_unref(texture);
    }
    const char *root = g_getenv("APPDIR");
    if (root) webkit_web_context_add_path_to_sandbox(webkit_web_context_get_default(), root, TRUE);
    GtkWidget *window = gtk_window_new();
    WebKitWebView *view = WEBKIT_WEB_VIEW(webkit_web_view_new());
    gtk_window_set_child(GTK_WINDOW(window), GTK_WIDGET(view));
    g_signal_connect(view, "load-changed", G_CALLBACK(loaded), NULL);
    loop = g_main_loop_new(NULL, FALSE);
    webkit_web_view_load_html(view, "<p>runtime probe</p><img id='image' src='data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jRZkAAAAASUVORK5CYII='>", "https://example.invalid/");
    gtk_window_present(GTK_WINDOW(window));
    g_timeout_add_seconds(25, expired, NULL);
    g_main_loop_run(loop);
    return result;
}
