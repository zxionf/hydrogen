/*
 * Android device smoke test for the shio-ffi C ABI.
 *
 * Build with the Android NDK, push next to libshio_ffi.so, then run:
 *   adb shell "cd /data/local/tmp/shio-smoke && LD_LIBRARY_PATH=. \
 *       ./android_smoke /data/local/tmp/shio-smoke/smoke.db"
 */

#include "shio.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static const char *PASSWORD = "android-smoke-password";
static const char *NEW_PASSWORD = "android-smoke-password-2";

static void fail(int32_t status, const char *what)
{
    char error[1024];
    size_t len = shio_last_error(NULL, error, sizeof(error));
    fprintf(stderr, "%s failed: status=%d error=%s\n",
            what, (int)status, len > 0 ? error : "(none)");
    exit(1);
}

static void expect(int32_t status, const char *what)
{
    if (status != SHIO_OK) {
        fail(status, what);
    }
}

static char *take_text(uint8_t *buf, size_t len, const char *what)
{
    char *text;

    if (buf == NULL || len == 0) {
        fprintf(stderr, "%s returned an empty buffer\n", what);
        exit(1);
    }
    text = (char *)malloc(len + 1);
    if (text == NULL) {
        fprintf(stderr, "out of memory\n");
        exit(1);
    }
    memcpy(text, buf, len);
    text[len] = '\0';
    shio_free(buf, len);
    return text;
}

static int contains(const char *text, const char *needle, const char *what)
{
    if (strstr(text, needle) == NULL) {
        fprintf(stderr, "%s does not contain %s: %s\n", what, needle, text);
        exit(1);
    }
    return 1;
}

static void uuid_text(const uint8_t id[16], char out[37])
{
    snprintf(out, 37,
             "%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-%02x%02x%02x%02x%02x%02x",
             id[0], id[1], id[2], id[3], id[4], id[5], id[6], id[7],
             id[8], id[9], id[10], id[11], id[12], id[13], id[14], id[15]);
}

int main(int argc, char **argv)
{
    const char *path;
    const char *mode;
    const char *options = "{\"create\":true}";
    shio_kernel *kernel = NULL;
    shio_kernel *second = NULL;
    uint8_t id[16];
    uint8_t explicit_id[16];
    uint8_t delete_id[16];
    uint8_t *buf = NULL;
    size_t len = 0;
    int32_t status;
    int32_t existed = 0;
    char id_text[37];
    char explicit_text[37];
    char batch[512];
    char *text;

    if (argc < 2 || argc > 3) {
        fprintf(stderr, "usage: %s <database-path> [hold|probe|check]\n", argv[0]);
        return 2;
    }
    path = argv[1];
    mode = argc == 3 ? argv[2] : "smoke";

    if (strcmp(mode, "hold") == 0) {
        remove(path);
        expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                         (const uint8_t *)options, strlen(options), &kernel),
               "shio_open(hold)");
        expect(shio_insert(kernel, "smoke", "items",
                           (const uint8_t *)"{\"name\":\"held\"}",
                           strlen("{\"name\":\"held\"}"), id, &buf, &len),
               "shio_insert(hold)");
        shio_free(buf, len);
        expect(shio_flush(kernel), "shio_flush(hold)");
        printf("HOLD_READY\n");
        fflush(stdout);
        sleep(20);
        expect(shio_close(kernel), "shio_close(hold)");
        shio_kernel_destroy(kernel);
        remove(path);
        printf("HOLD_DONE\n");
        return 0;
    }

    if (strcmp(mode, "probe") == 0) {
        status = shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                           NULL, 0, &kernel);
        if (status == SHIO_LOCKED) {
            printf("PROBE_LOCKED\n");
            return 0;
        }
        if (status == SHIO_OK) {
            printf("PROBE_UNEXPECTED_OK\n");
            expect(shio_close(kernel), "shio_close(probe)");
            shio_kernel_destroy(kernel);
            return 1;
        }
        fail(status, "shio_open(probe)");
    }

    if (strcmp(mode, "check") == 0) {
        expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                         NULL, 0, &kernel),
               "shio_open(check)");
        expect(shio_stats(kernel, &buf, &len), "shio_stats(check)");
        shio_free(buf, len);
        expect(shio_close(kernel), "shio_close(check)");
        shio_kernel_destroy(kernel);
        printf("CHECK_OK\n");
        return 0;
    }

    if (strcmp(mode, "smoke") != 0) {
        fprintf(stderr, "unknown mode: %s\n", mode);
        return 2;
    }

    remove(path);

    if (shio_abi_version() != SHIO_ABI_VERSION) {
        fprintf(stderr, "ABI mismatch: runtime=%u header=%u\n",
                shio_abi_version(), SHIO_ABI_VERSION);
        return 1;
    }

    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)options, strlen(options), &kernel),
           "shio_open(create)");

    expect(shio_stats(kernel, &buf, &len), "shio_stats");
    text = take_text(buf, len, "shio_stats");
    printf("stats after create: %s\n", text);
    contains(text, "\"format_version\"", "stats");
    free(text);

    expect(shio_insert(kernel, "smoke", "items",
                       (const uint8_t *)"{\"name\":\"alpha\",\"n\":1}",
                       strlen("{\"name\":\"alpha\",\"n\":1}"),
                       id, &buf, &len),
           "shio_insert");
    text = take_text(buf, len, "shio_insert meta");
    printf("insert meta: %s\n", text);
    contains(text, "\"id\"", "insert meta");
    free(text);
    uuid_text(id, id_text);

    memset(explicit_id, 0x42, sizeof(explicit_id));
    uuid_text(explicit_id, explicit_text);
    expect(shio_put(kernel, "smoke", "items", explicit_id,
                    (const uint8_t *)"{\"name\":\"beta\",\"n\":2}",
                    strlen("{\"name\":\"beta\",\"n\":2}"),
                    &buf, &len),
           "shio_put");
    shio_free(buf, len);

    expect(shio_get(kernel, "smoke", "items", id, &buf, &len), "shio_get");
    text = take_text(buf, len, "shio_get");
    printf("get before batch: %s\n", text);
    contains(text, "alpha", "get");
    free(text);

    snprintf(batch, sizeof(batch),
             "[{\"id\":\"%s\",\"value\":{\"name\":\"alpha2\",\"n\":3}},"
             "{\"id\":\"%s\",\"delete\":true}]",
             id_text, explicit_text);
    expect(shio_batch(kernel, "smoke", "items",
                      (const uint8_t *)batch, strlen(batch), &buf, &len),
           "shio_batch");
    shio_free(buf, len);

    expect(shio_get(kernel, "smoke", "items", id, &buf, &len),
           "shio_get(after batch)");
    text = take_text(buf, len, "shio_get(after batch)");
    printf("get after batch: %s\n", text);
    contains(text, "alpha2", "get after batch");
    free(text);

    status = shio_get(kernel, "smoke", "items", explicit_id, &buf, &len);
    if (status != SHIO_NOT_FOUND) {
        fail(status, "shio_get(deleted)");
    }
    printf("deleted record is not found, error detail: %s\n",
           shio_last_error(NULL, NULL, 0) > 0 ? "(present)" : "(missing)");

    memset(delete_id, 0x43, sizeof(delete_id));
    expect(shio_put(kernel, "smoke", "items", delete_id,
                    (const uint8_t *)"{\"name\":\"temp\"}",
                    strlen("{\"name\":\"temp\"}"), &buf, &len),
           "shio_put(temp)");
    shio_free(buf, len);
    expect(shio_delete(kernel, "smoke", "items", delete_id, &existed),
           "shio_delete");
    if (existed != 1) {
        fprintf(stderr, "shio_delete reported existed=%d, expected 1\n",
                (int)existed);
        return 1;
    }
    status = shio_get(kernel, "smoke", "items", delete_id, &buf, &len);
    if (status != SHIO_NOT_FOUND) {
        fail(status, "shio_get(directly deleted)");
    }

    expect(shio_list(kernel, "smoke", "items", 0, 10, 0, &buf, &len),
           "shio_list");
    text = take_text(buf, len, "shio_list");
    printf("list: %s\n", text);
    contains(text, "\"items\"", "list");
    contains(text, id_text, "list");
    free(text);

    expect(shio_rotate_data_key(kernel), "shio_rotate_data_key");
    expect(shio_flush(kernel), "shio_flush");

    status = shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                       NULL, 0, &second);
    if (status == SHIO_LOCKED) {
        printf("second writer open correctly rejected with SHIO_LOCKED\n");
    } else if (status == SHIO_OK) {
        printf("warning: second writer open succeeded\n");
        expect(shio_close(second), "shio_close(second)");
        shio_kernel_destroy(second);
        second = NULL;
    } else {
        fail(status, "shio_open(second writer)");
    }

    expect(shio_close(kernel), "shio_close");
    expect(shio_close(kernel), "shio_close(again)");
    status = shio_flush(kernel);
    if (status != SHIO_CLOSED) {
        fail(status, "shio_flush(closed)");
    }
    shio_kernel_destroy(kernel);
    kernel = NULL;

    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     NULL, 0, &kernel),
           "shio_open(reopen)");
    expect(shio_get(kernel, "smoke", "items", id, &buf, &len),
           "shio_get(reopen)");
    text = take_text(buf, len, "shio_get(reopen)");
    printf("get after reopen: %s\n", text);
    contains(text, "alpha2", "get after reopen");
    free(text);

    expect(shio_change_password(kernel,
                                (const uint8_t *)NEW_PASSWORD, strlen(NEW_PASSWORD)),
           "shio_change_password");
    expect(shio_close(kernel), "shio_close(before read-only)");
    shio_kernel_destroy(kernel);
    kernel = NULL;

    expect(shio_open(path, (const uint8_t *)NEW_PASSWORD, strlen(NEW_PASSWORD),
                     NULL, 0, &kernel),
           "shio_open(new password)");
    expect(shio_close(kernel), "shio_close(new password)");
    shio_kernel_destroy(kernel);
    kernel = NULL;

    expect(shio_open(path, (const uint8_t *)NEW_PASSWORD, strlen(NEW_PASSWORD),
                     (const uint8_t *)"{\"read_only\":true}",
                     strlen("{\"read_only\":true}"), &kernel),
           "shio_open(read_only)");
    status = shio_put(kernel, "smoke", "items", id,
                      (const uint8_t *)"{\"name\":\"blocked\"}",
                      strlen("{\"name\":\"blocked\"}"), &buf, &len);
    if (status != SHIO_READ_ONLY) {
        fail(status, "shio_put(read_only)");
    }
    printf("read-only write correctly rejected with SHIO_READ_ONLY\n");
    expect(shio_close(kernel), "shio_close(read_only)");
    shio_kernel_destroy(kernel);

    remove(path);
    printf("ANDROID_SMOKE_OK\n");
    return 0;
}
