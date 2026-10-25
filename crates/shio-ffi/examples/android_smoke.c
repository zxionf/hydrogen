/*
 * Android device smoke test for the shio-ffi C ABI.
 *
 * Build with the Android NDK, push next to libshio_ffi.so, then run:
 *   adb shell "cd /data/local/tmp/shio-smoke && LD_LIBRARY_PATH=. \
 *       ./android_smoke /data/local/tmp/shio-smoke/smoke.db"
 */

#include "shio.h"

#include <pthread.h>
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

static uint8_t *take_bytes(uint8_t *buf, size_t len)
{
    uint8_t *copy;

    if (buf == NULL || len == 0) {
        return NULL;
    }
    copy = (uint8_t *)malloc(len);
    if (copy == NULL) {
        fprintf(stderr, "out of memory\n");
        exit(1);
    }
    memcpy(copy, buf, len);
    shio_free(buf, len);
    return copy;
}

static char *json_string(const char *json, const char *key, const char *what)
{
    char needle[64];
    const char *start;
    const char *end;
    char *value;
    size_t len;

    snprintf(needle, sizeof(needle), "\"%s\":\"", key);
    start = strstr(json, needle);
    if (start == NULL) {
        fprintf(stderr, "%s missing key %s: %s\n", what, key, json);
        exit(1);
    }
    start += strlen(needle);
    end = strchr(start, '"');
    if (end == NULL) {
        fprintf(stderr, "%s has an unterminated key %s: %s\n", what, key, json);
        exit(1);
    }
    len = (size_t)(end - start);
    value = (char *)malloc(len + 1);
    if (value == NULL) {
        fprintf(stderr, "out of memory\n");
        exit(1);
    }
    memcpy(value, start, len);
    value[len] = '\0';
    return value;
}

static uint64_t json_u64(const char *json, const char *key, const char *what)
{
    char needle[64];
    const char *start;
    char *end = NULL;
    unsigned long long value;

    snprintf(needle, sizeof(needle), "\"%s\":", key);
    start = strstr(json, needle);
    if (start == NULL) {
        fprintf(stderr, "%s missing key %s: %s\n", what, key, json);
        exit(1);
    }
    start += strlen(needle);
    value = strtoull(start, &end, 10);
    if (end == start) {
        fprintf(stderr, "%s has a bad key %s: %s\n", what, key, json);
        exit(1);
    }
    return (uint64_t)value;
}

static void hex_decode_16(const char *hex, uint8_t out[16])
{
    size_t i;

    if (strlen(hex) != 32) {
        fprintf(stderr, "bad 16-byte hex value: %s\n", hex);
        exit(1);
    }
    for (i = 0; i < 16; i++) {
        unsigned int byte = 0;
        if (sscanf(hex + i * 2, "%2x", &byte) != 1) {
            fprintf(stderr, "bad hex value: %s\n", hex);
            exit(1);
        }
        out[i] = (uint8_t)byte;
    }
}

static void uuid_text(const uint8_t id[16], char out[37])
{
    snprintf(out, 37,
             "%02x%02x%02x%02x-%02x%02x-%02x%02x-%02x%02x-%02x%02x%02x%02x%02x%02x",
             id[0], id[1], id[2], id[3], id[4], id[5], id[6], id[7],
             id[8], id[9], id[10], id[11], id[12], id[13], id[14], id[15]);
}

static char *sync_info_text(shio_kernel *kernel, const char *what)
{
    uint8_t *buf = NULL;
    size_t len = 0;

    expect(shio_sync_info(kernel, &buf, &len), what);
    return take_text(buf, len, what);
}

static char *migration_progress_text(shio_kernel *kernel, const char *what)
{
    uint8_t *buf = NULL;
    size_t len = 0;

    expect(shio_sync_migration_progress(kernel, &buf, &len), what);
    return take_text(buf, len, what);
}

static void pull_changes(shio_kernel *from, shio_kernel *to, const char *what)
{
    uint64_t after = 0;
    int pages = 0;

    for (;;) {
        uint8_t *frame = NULL;
        size_t frame_len = 0;
        uint64_t next = 0;
        uint8_t more = 0;
        uint8_t *report = NULL;
        size_t report_len = 0;

        expect(shio_sync_changes(from, after, 2, &frame, &frame_len, &next, &more),
               what);
        if (frame == NULL || frame_len == 0) {
            fprintf(stderr, "%s returned an empty frame\n", what);
            exit(1);
        }
        expect(shio_sync_apply(to, frame, frame_len, &report, &report_len), what);
        shio_free(frame, frame_len);
        if (report != NULL) {
            shio_free(report, report_len);
        }
        pages++;
        after = next;
        if (!more) {
            break;
        }
        if (pages > 10000) {
            fprintf(stderr, "%s did not terminate\n", what);
            exit(1);
        }
    }
    printf("%s: pages=%d\n", what, pages);
}

static void copy_snapshot(shio_kernel *from, shio_kernel *to, const char *what)
{
    uint8_t *cursor = NULL;
    size_t cursor_len = 0;
    int pages = 0;

    for (;;) {
        uint8_t *frame = NULL;
        size_t frame_len = 0;
        uint8_t *next = NULL;
        size_t next_len = 0;
        uint8_t done = 0;
        uint8_t *report = NULL;
        size_t report_len = 0;

        expect(shio_sync_snapshot_page(from, cursor, cursor_len, 2,
                                      &frame, &frame_len, &next, &next_len,
                                      &done),
               what);
        if (cursor != NULL) {
            shio_free(cursor, cursor_len);
            cursor = NULL;
            cursor_len = 0;
        }
        if (frame == NULL || frame_len == 0) {
            fprintf(stderr, "%s returned an empty frame\n", what);
            exit(1);
        }
        expect(shio_sync_apply_snapshot(to, frame, frame_len, &report, &report_len),
               what);
        shio_free(frame, frame_len);
        if (report != NULL) {
            shio_free(report, report_len);
        }
        pages++;
        if (done) {
            if (next != NULL) {
                shio_free(next, next_len);
            }
            break;
        }
        if (next == NULL || next_len == 0) {
            fprintf(stderr, "%s is not done but has no cursor\n", what);
            exit(1);
        }
        cursor = take_bytes(next, next_len);
        cursor_len = next_len;
        if (pages > 10000) {
            fprintf(stderr, "%s did not terminate\n", what);
            exit(1);
        }
    }
    printf("%s: pages=%d\n", what, pages);
}

#define ROTATE_TOTAL 60000
#define ROTATE_CHUNK 500

static void record_id(int n, uint8_t out[16])
{
    memset(out, 0, 16);
    out[0] = 0xa5;
    out[12] = (uint8_t)((unsigned int)n >> 24);
    out[13] = (uint8_t)((unsigned int)n >> 16);
    out[14] = (uint8_t)((unsigned int)n >> 8);
    out[15] = (uint8_t)n;
}

static int rotate_many(const char *path)
{
    const char *options = "{\"create\":true}";
    char lock_path[1024];
    char *ops;
    const size_t capacity = 256 * 1024;
    shio_kernel *kernel = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t id[16];
    char id_text[37];
    int base;

    snprintf(lock_path, sizeof(lock_path), "%s.lock", path);
    remove(path);
    remove(lock_path);
    ops = (char *)malloc(capacity);
    if (ops == NULL) {
        fprintf(stderr, "out of memory\n");
        return 1;
    }

    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)options, strlen(options), &kernel),
           "rotate open");
    for (base = 0; base < ROTATE_TOTAL; base += ROTATE_CHUNK) {
        int count = ROTATE_TOTAL - base;
        size_t off = 0;
        int j;

        if (count > ROTATE_CHUNK) {
            count = ROTATE_CHUNK;
        }
        off += (size_t)snprintf(ops + off, capacity - off, "[");
        for (j = 0; j < count; j++) {
            char entry[160];
            int written;

            record_id(base + j, id);
            uuid_text(id, id_text);
            written = snprintf(entry, sizeof(entry),
                               "%s{\"id\":\"%s\",\"value\":{\"n\":%d}}",
                               j == 0 ? "" : ",", id_text, base + j);
            if (written < 0 || (size_t)written >= sizeof(entry) ||
                off + (size_t)written >= capacity) {
                fprintf(stderr, "rotate batch is too large\n");
                return 1;
            }
            memcpy(ops + off, entry, (size_t)written);
            off += (size_t)written;
        }
        if (off + 2 >= capacity) {
            fprintf(stderr, "rotate batch terminator is too large\n");
            return 1;
        }
        ops[off++] = ']';
        ops[off] = '\0';
        expect(shio_batch(kernel, "rotate", "items",
                          (const uint8_t *)ops, off, &buf, &len),
               "rotate batch");
        shio_free(buf, len);
        buf = NULL;
        len = 0;
    }
    free(ops);
    expect(shio_flush(kernel), "rotate flush");
    printf("ROTATE_READY\n");
    fflush(stdout);
    printf("ROTATE_START\n");
    fflush(stdout);
    expect(shio_rotate_data_key(kernel), "rotate_data_key");
    printf("ROTATE_DONE\n");
    fflush(stdout);

    record_id(0, id);
    expect(shio_get(kernel, "rotate", "items", id, &buf, &len),
           "rotate get first");
    shio_free(buf, len);
    record_id(ROTATE_TOTAL - 1, id);
    expect(shio_get(kernel, "rotate", "items", id, &buf, &len),
           "rotate get last");
    shio_free(buf, len);
    expect(shio_close(kernel), "rotate close");
    shio_kernel_destroy(kernel);
    printf("ROTATE_MANY_OK\n");
    return 0;
}

static int resume_rotation(const char *path)
{
    shio_kernel *kernel = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t id[16];
    uint8_t to[16];
    uint8_t *json = NULL;
    size_t json_len = 0;
    char *text = NULL;
    char *to_text = NULL;

    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     NULL, 0, &kernel),
           "resume open");
    expect(shio_sync_rotation_state(kernel, &json, &json_len),
           "resume rotation_state");
    text = take_text(json, json_len, "resume rotation_state");
    if (strcmp(text, "null") == 0) {
        fprintf(stderr, "resume rotation_state returned null\n");
        return 1;
    }
    to_text = json_string(text, "to", "resume rotation_state");
    hex_decode_16(to_text, to);
    free(to_text);
    free(text);
    expect(shio_sync_migrate_all_ex(kernel, to, 0, 0), "resume migrate_all_ex");
    text = migration_progress_text(kernel, "resume migration_progress");
    contains(text, "\"done\":true", "resume migration_progress");
    free(text);

    record_id(0, id);
    expect(shio_get(kernel, "rotate", "items", id, &buf, &len),
           "resume get first");
    shio_free(buf, len);
    record_id(ROTATE_TOTAL - 1, id);
    expect(shio_get(kernel, "rotate", "items", id, &buf, &len),
           "resume get last");
    shio_free(buf, len);

    memset(id, 0x11, sizeof(id));
    expect(shio_put(kernel, "rotate", "items", id,
                    (const uint8_t *)"{\"after\":\"resume\"}",
                    strlen("{\"after\":\"resume\"}"), &buf, &len),
           "resume put");
    shio_free(buf, len);
    expect(shio_close(kernel), "resume close");
    shio_kernel_destroy(kernel);
    printf("ROTATE_RESUME_OK\n");
    return 0;
}

typedef struct {
    shio_kernel *kernel;
    uint8_t to[16];
    int32_t status;
} migration_job;

static void *migration_worker(void *arg)
{
    migration_job *job = (migration_job *)arg;
    job->status = shio_sync_migrate_all_ex(job->kernel, job->to, 1, 20);
    return NULL;
}

static int migration_progress_mode(const char *path)
{
    shio_kernel *kernel = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t id[16];
    uint8_t to[16];
    uint8_t *json = NULL;
    size_t json_len = 0;
    char *text = NULL;
    char *to_text = NULL;
    migration_job job;
    pthread_t thread;
    int waited = 0;
    int saw_progress = 0;

    remove(path);
    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)"{\"create\":true}",
                     strlen("{\"create\":true}"), &kernel),
           "migration open");
    expect(shio_sync_enable(kernel, NULL, &buf, &len), "migration enable");
    shio_free(buf, len);
    buf = NULL;
    len = 0;

    for (waited = 0; waited < 40; waited++) {
        char payload[64];
        snprintf(payload, sizeof(payload), "{\"n\":%d}", waited);
        expect(shio_insert(kernel, "migration", "items",
                           (const uint8_t *)payload, strlen(payload),
                           id, &buf, &len),
               "migration insert");
        shio_free(buf, len);
        buf = NULL;
        len = 0;
    }

    expect(shio_sync_begin_rotation(kernel, &json, &json_len),
           "migration begin_rotation");
    text = take_text(json, json_len, "migration begin_rotation");
    to_text = json_string(text, "to", "migration begin_rotation");
    hex_decode_16(to_text, to);
    free(to_text);
    free(text);

    job.kernel = kernel;
    memcpy(job.to, to, sizeof(to));
    job.status = SHIO_OK;
    if (pthread_create(&thread, NULL, migration_worker, &job) != 0) {
        fprintf(stderr, "pthread_create failed\n");
        return 1;
    }

    for (waited = 0; waited < 5000; waited++) {
        uint64_t migrated;
        text = migration_progress_text(kernel, "migration progress");
        migrated = json_u64(text, "migrated", "migration progress");
        if (strstr(text, "\"active\":true") != NULL && migrated >= 1) {
            saw_progress = 1;
            free(text);
            break;
        }
        free(text);
        usleep(2000);
    }
    if (!saw_progress) {
        fprintf(stderr, "migration did not report active progress\n");
        return 1;
    }

    expect(shio_sync_cancel_migration(kernel), "migration cancel");
    pthread_join(thread, NULL);
    if (job.status != SHIO_CANCELLED) {
        fail(job.status, "migration worker");
    }

    text = migration_progress_text(kernel, "migration progress(cancelled)");
    contains(text, "\"active\":false", "migration progress(cancelled)");
    contains(text, "\"done\":false", "migration progress(cancelled)");
    free(text);

    expect(shio_sync_migrate_all_ex(kernel, to, 0, 0), "migration resume");
    text = migration_progress_text(kernel, "migration progress(done)");
    contains(text, "\"active\":false", "migration progress(done)");
    contains(text, "\"done\":true", "migration progress(done)");
    free(text);

    expect(shio_close(kernel), "migration close");
    shio_kernel_destroy(kernel);
    remove(path);
    printf("MIGRATION_PROGRESS_OK\n");
    return 0;
}

static int maintenance_mode(const char *path)
{
    shio_kernel *kernel = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t id[16];
    uint8_t to[16];
    uint8_t *json = NULL;
    size_t json_len = 0;
    char *text = NULL;
    char *to_text = NULL;
    uint8_t done = 0;
    int32_t status;
    int index = 0;
    int steps = 0;

    remove(path);
    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)"{\"create\":true}",
                     strlen("{\"create\":true}"), &kernel),
           "maintenance open");
    expect(shio_sync_enable(kernel, NULL, &buf, &len), "maintenance enable");
    shio_free(buf, len);
    buf = NULL;
    len = 0;

    for (index = 0; index < 24; index++) {
        char payload[48];
        snprintf(payload, sizeof(payload), "{\"n\":%d}", index);
        record_id(index, id);
        expect(shio_put(kernel, "maintenance", "entries", id,
                        (const uint8_t *)payload, strlen(payload), &buf, &len),
               "maintenance put");
        shio_free(buf, len);
        buf = NULL;
        len = 0;
    }

    expect(shio_sync_begin_rotation(kernel, &json, &json_len),
           "maintenance begin_rotation");
    text = take_text(json, json_len, "maintenance begin_rotation");
    to_text = json_string(text, "to", "maintenance begin_rotation");
    hex_decode_16(to_text, to);
    free(to_text);
    free(text);
    json = NULL;
    json_len = 0;

    expect(shio_sync_begin_maintenance(kernel), "maintenance begin");

    /* 维护窗口内普通写被拒绝，且不产生输出缓冲区。 */
    record_id(0, id);
    status = shio_put(kernel, "maintenance", "entries", id,
                      (const uint8_t *)"{\"n\":99}", strlen("{\"n\":99}"),
                      &buf, &len);
    if (status != SHIO_BUSY) {
        fprintf(stderr, "maintenance put returned %d instead of SHIO_BUSY\n",
                (int)status);
        return 1;
    }
    if (buf != NULL || len != 0) {
        fprintf(stderr, "maintenance put returned a buffer\n");
        return 1;
    }

    /* 读与集合枚举不受维护模式影响。 */
    expect(shio_get(kernel, "maintenance", "entries", id, &buf, &len),
           "maintenance get");
    text = take_text(buf, len, "maintenance get");
    contains(text, "\"n\":0", "maintenance get");
    free(text);
    expect(shio_collections(kernel, "maintenance", &buf, &len),
           "maintenance collections");
    text = take_text(buf, len, "maintenance collections");
    contains(text, "\"entries\"", "maintenance collections");
    free(text);

    /* 同步启用时物理删除先于维护窗口拒绝。 */
    status = shio_purge(kernel, "maintenance", "entries", id, NULL);
    if (status != SHIO_SYNC_ENABLED) {
        fprintf(stderr, "maintenance purge returned %d instead of SHIO_SYNC_ENABLED\n",
                (int)status);
        return 1;
    }

    /* 每步 5 条，直到 done。 */
    for (steps = 0; steps < 20; steps++) {
        uint64_t scanned;
        expect(shio_sync_migrate_step(kernel, to, 5, &json, &json_len),
               "maintenance migrate_step");
        text = take_text(json, json_len, "maintenance migrate_step");
        json = NULL;
        json_len = 0;
        scanned = json_u64(text, "scanned", "maintenance migrate_step");
        if (scanned > 5) {
            fprintf(stderr, "maintenance step scanned %llu records\n",
                    (unsigned long long)scanned);
            free(text);
            return 1;
        }
        if (strstr(text, "\"done\":true") != NULL) {
            free(text);
            break;
        }
        free(text);
    }
    if (steps >= 20) {
        fprintf(stderr, "maintenance step migration did not finish\n");
        return 1;
    }
    printf("maintenance steps: %d\n", steps + 1);

    expect(shio_sync_end_maintenance(kernel, &done), "maintenance end");
    if (done != 1) {
        fprintf(stderr, "maintenance end reported done=%u\n", (unsigned)done);
        return 1;
    }

    /* 退出后普通写恢复，并读到新值。 */
    expect(shio_put(kernel, "maintenance", "entries", id,
                    (const uint8_t *)"{\"n\":100}", strlen("{\"n\":100}"),
                    &buf, &len),
           "maintenance put(after)");
    shio_free(buf, len);
    buf = NULL;
    len = 0;
    expect(shio_get(kernel, "maintenance", "entries", id, &buf, &len),
           "maintenance get(after)");
    text = take_text(buf, len, "maintenance get(after)");
    contains(text, "\"n\":100", "maintenance get(after)");
    free(text);

    expect(shio_close(kernel), "maintenance close");
    shio_kernel_destroy(kernel);
    remove(path);
    printf("MAINTENANCE_OK\n");
    return 0;
}

static int cooperative_mode(const char *path)
{
    shio_kernel *kernel = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t id[16];
    uint8_t to[16];
    uint8_t *json = NULL;
    size_t json_len = 0;
    char *text = NULL;
    char *to_text = NULL;
    int32_t status;
    int index = 0;
    int ticks = 0;
    uint64_t scanned = 0;

    remove(path);
    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)"{\"create\":true}",
                     strlen("{\"create\":true}"), &kernel),
           "cooperative open");
    expect(shio_sync_enable(kernel, NULL, &buf, &len), "cooperative enable");
    shio_free(buf, len);
    buf = NULL;
    len = 0;

    for (index = 0; index < 18; index++) {
        char payload[48];
        snprintf(payload, sizeof(payload), "{\"n\":%d}", index);
        record_id(index, id);
        expect(shio_put(kernel, "cooperative", "entries", id,
                        (const uint8_t *)payload, strlen(payload), &buf, &len),
               "cooperative put");
        shio_free(buf, len);
        buf = NULL;
        len = 0;
    }

    expect(shio_sync_begin_rotation(kernel, &json, &json_len),
           "cooperative begin_rotation");
    text = take_text(json, json_len, "cooperative begin_rotation");
    to_text = json_string(text, "to", "cooperative begin_rotation");
    hex_decode_16(to_text, to);
    free(to_text);
    free(text);
    json = NULL;
    json_len = 0;

    /* 不进入维护模式：单次 tick 只推进一个小批次。 */
    expect(shio_sync_migrate_tick(kernel, to, 1, 4, &json, &json_len),
           "cooperative tick");
    text = take_text(json, json_len, "cooperative tick");
    json = NULL;
    json_len = 0;
    scanned = json_u64(text, "scanned", "cooperative tick");
    if (scanned < 1) {
        fprintf(stderr, "cooperative tick scanned no records\n");
        free(text);
        return 1;
    }
    free(text);

    /* tick 之间普通写不被拒绝，并可立即读回。 */
    record_id(100, id);
    expect(shio_put(kernel, "cooperative", "entries", id,
                    (const uint8_t *)"{\"n\":100}", strlen("{\"n\":100}"),
                    &buf, &len),
           "cooperative put(between)");
    shio_free(buf, len);
    buf = NULL;
    len = 0;
    expect(shio_get(kernel, "cooperative", "entries", id, &buf, &len),
           "cooperative get(between)");
    text = take_text(buf, len, "cooperative get(between)");
    contains(text, "\"n\":100", "cooperative get(between)");
    free(text);

    /* 继续 tick 直到 done。 */
    for (ticks = 0; ticks < 100; ticks++) {
        expect(shio_sync_migrate_tick(kernel, to, 1, 4, &json, &json_len),
               "cooperative tick");
        text = take_text(json, json_len, "cooperative tick");
        json = NULL;
        json_len = 0;
        if (strstr(text, "\"done\":true") != NULL) {
            free(text);
            break;
        }
        free(text);
    }
    if (ticks >= 100) {
        fprintf(stderr, "cooperative tick migration did not finish\n");
        return 1;
    }
    printf("cooperative ticks: %d\n", ticks + 2);

    text = migration_progress_text(kernel, "cooperative progress");
    contains(text, "\"active\":false", "cooperative progress");
    contains(text, "\"done\":true", "cooperative progress");
    free(text);

    /* 迁移完成后所有记录仍可读。 */
    record_id(0, id);
    expect(shio_get(kernel, "cooperative", "entries", id, &buf, &len),
           "cooperative get(first)");
    text = take_text(buf, len, "cooperative get(first)");
    contains(text, "\"n\":0", "cooperative get(first)");
    free(text);
    record_id(17, id);
    expect(shio_get(kernel, "cooperative", "entries", id, &buf, &len),
           "cooperative get(last)");
    text = take_text(buf, len, "cooperative get(last)");
    contains(text, "\"n\":17", "cooperative get(last)");
    free(text);

    /* 预算与批次配额越界在写事务前被拒绝。 */
    status = shio_sync_migrate_tick(kernel, to, 1001, 4, &json, &json_len);
    if (status != SHIO_INVALID_ARGUMENT) {
        fprintf(stderr, "cooperative tick(budget) returned %d\n", (int)status);
        return 1;
    }
    status = shio_sync_migrate_tick(kernel, to, 10, 257, &json, &json_len);
    if (status != SHIO_INVALID_ARGUMENT) {
        fprintf(stderr, "cooperative tick(batch) returned %d\n", (int)status);
        return 1;
    }

    expect(shio_close(kernel), "cooperative close");
    shio_kernel_destroy(kernel);
    remove(path);
    printf("COOPERATIVE_OK\n");
    return 0;
}

static int compact_mode(const char *path)
{
    shio_kernel *kernel = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t id[16];
    int32_t existed = 0;
    uint8_t *json = NULL;
    size_t json_len = 0;
    char *text = NULL;
    int32_t status;
    int index = 0;
    uint64_t before = 0;
    uint64_t after = 0;

    remove(path);
    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)"{\"create\":true}",
                     strlen("{\"create\":true}"), &kernel),
           "compact open");
    for (index = 0; index < 40; index++) {
        char payload[48];
        snprintf(payload, sizeof(payload), "{\"n\":%d}", index);
        record_id(index, id);
        expect(shio_put(kernel, "compact", "entries", id,
                        (const uint8_t *)payload, strlen(payload), &buf, &len),
               "compact put");
        shio_free(buf, len);
        buf = NULL;
        len = 0;
    }

    /* 删除大部分记录，制造可回收的空闲页。 */
    for (index = 1; index < 40; index++) {
        record_id(index, id);
        expect(shio_delete(kernel, "compact", "entries", id, &existed),
               "compact delete");
        if (existed != 1) {
            fprintf(stderr, "compact delete reported existed=%d\n",
                    (int)existed);
            return 1;
        }
    }

    /* 句柄未关闭时压缩被拒绝，且不产生输出缓冲区。 */
    status = shio_compact_file(path, &json, &json_len);
    if (status != SHIO_LOCKED) {
        fprintf(stderr, "compact while open returned %d instead of SHIO_LOCKED\n",
                (int)status);
        return 1;
    }
    if (json != NULL || json_len != 0) {
        fprintf(stderr, "compact while open returned a buffer\n");
        return 1;
    }

    expect(shio_close(kernel), "compact close");
    shio_kernel_destroy(kernel);
    kernel = NULL;

    expect(shio_compact_file(path, &json, &json_len), "compact file");
    text = take_text(json, json_len, "compact file");
    json = NULL;
    json_len = 0;
    before = json_u64(text, "before_bytes", "compact file");
    after = json_u64(text, "after_bytes", "compact file");
    if (before == 0 || after >= before) {
        fprintf(stderr, "compact file sizes: %llu -> %llu\n",
                (unsigned long long)before, (unsigned long long)after);
        free(text);
        return 1;
    }
    if (strstr(text, "\"compacted\":true") == NULL) {
        fprintf(stderr, "compact file reported compacted=false\n");
        free(text);
        return 1;
    }
    free(text);
    printf("compact bytes: %llu -> %llu\n", (unsigned long long)before,
           (unsigned long long)after);

    /* 连续压缩不再有工作。 */
    expect(shio_compact_file(path, &json, &json_len), "compact file(again)");
    text = take_text(json, json_len, "compact file(again)");
    json = NULL;
    json_len = 0;
    if (strstr(text, "\"compacted\":false") == NULL) {
        fprintf(stderr, "second compact reported work\n");
        free(text);
        return 1;
    }
    free(text);

    /* 重开后保留记录可读，写入照常。 */
    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD), NULL, 0,
                     &kernel),
           "compact reopen");
    record_id(0, id);
    expect(shio_get(kernel, "compact", "entries", id, &buf, &len),
           "compact get");
    text = take_text(buf, len, "compact get");
    contains(text, "\"n\":0", "compact get");
    free(text);
    record_id(100, id);
    expect(shio_put(kernel, "compact", "entries", id,
                    (const uint8_t *)"{\"n\":100}", strlen("{\"n\":100}"),
                    &buf, &len),
           "compact put(after)");
    shio_free(buf, len);
    buf = NULL;
    len = 0;
    expect(shio_close(kernel), "compact close(after)");
    shio_kernel_destroy(kernel);
    remove(path);
    printf("COMPACT_OK\n");
    return 0;
}

static int backup_restore_mode(const char *path)
{
    shio_kernel *source = NULL;
    shio_kernel *restored = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t id[16];
    char backup_path[1024];
    char target_path[1024];
    char wrong_path[1024];
    char *text = NULL;
    char *source_db = NULL;
    char *source_device = NULL;
    char *restored_db = NULL;
    char *restored_device = NULL;
    int32_t status;

    if (snprintf(backup_path, sizeof(backup_path), "%s.backup", path) >=
            (int)sizeof(backup_path) ||
        snprintf(target_path, sizeof(target_path), "%s.restored", path) >=
            (int)sizeof(target_path) ||
        snprintf(wrong_path, sizeof(wrong_path), "%s.wrong", path) >=
            (int)sizeof(wrong_path)) {
        fprintf(stderr, "backup paths are too long\n");
        return 1;
    }
    remove(path);
    remove(backup_path);
    remove(target_path);
    remove(wrong_path);

    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)"{\"create\":true}",
                     strlen("{\"create\":true}"), &source),
           "backup open");
    expect(shio_sync_enable(source, NULL, &buf, &len), "backup enable_sync");
    shio_free(buf, len);
    buf = NULL;
    len = 0;
    expect(shio_insert(source, "backup", "items",
                       (const uint8_t *)"{\"name\":\"backup\"}",
                       strlen("{\"name\":\"backup\"}"), id, &buf, &len),
           "backup insert");
    shio_free(buf, len);
    buf = NULL;
    len = 0;

    text = sync_info_text(source, "backup sync_info(source)");
    source_db = json_string(text, "db_id", "backup sync_info(source)");
    source_device = json_string(text, "device_id", "backup sync_info(source)");
    free(text);

    expect(shio_backup(source, backup_path, &buf, &len), "shio_backup");
    text = take_text(buf, len, "shio_backup");
    printf("backup: %s\n", text);
    if (json_u64(text, "row_count", "shio_backup") < 1 ||
        json_u64(text, "format_version", "shio_backup") != 2) {
        fprintf(stderr, "unexpected backup summary: %s\n", text);
        return 1;
    }
    free(text);
    buf = NULL;
    len = 0;

    /* 目标已存在时拒绝覆盖。 */
    status = shio_backup(source, backup_path, &buf, &len);
    if (status != SHIO_INVALID_ARGUMENT) {
        fail(status, "shio_backup(existing)");
    }
    if (buf != NULL || len != 0) {
        fprintf(stderr, "backup(existing) returned a buffer\n");
        return 1;
    }

    /* 非空目标不得被恢复覆盖。 */
    {
        FILE *target_file = fopen(target_path, "wb");
        if (target_file == NULL) {
            fprintf(stderr, "cannot create restore target\n");
            return 1;
        }
        fputs("keep", target_file);
        fclose(target_file);
    }
    status = shio_restore(backup_path, target_path,
                          (const uint8_t *)PASSWORD, strlen(PASSWORD),
                          NULL, 0, &restored);
    if (status != SHIO_INVALID_ARGUMENT || restored != NULL) {
        fail(status, "shio_restore(nonempty)");
    }
    remove(target_path);

    /* 错误口令不得发布目标。 */
    status = shio_restore(backup_path, wrong_path,
                          (const uint8_t *)"wrong-password",
                          strlen("wrong-password"), NULL, 0, &restored);
    if (status != SHIO_INVALID_KEY || restored != NULL) {
        fail(status, "shio_restore(wrong password)");
    }
    if (access(wrong_path, F_OK) == 0) {
        fprintf(stderr, "wrong-password restore left a target file\n");
        return 1;
    }

    expect(shio_restore(backup_path, target_path,
                        (const uint8_t *)PASSWORD, strlen(PASSWORD),
                        NULL, 0, &restored),
           "shio_restore");
    expect(shio_get(restored, "backup", "items", id, &buf, &len),
           "restored get");
    text = take_text(buf, len, "restored get");
    contains(text, "\"backup\"", "restored get");
    free(text);
    buf = NULL;
    len = 0;

    text = sync_info_text(restored, "backup sync_info(restored)");
    restored_db = json_string(text, "db_id", "backup sync_info(restored)");
    restored_device =
        json_string(text, "device_id", "backup sync_info(restored)");
    free(text);
    if (strcmp(source_db, restored_db) != 0 ||
        strcmp(source_device, restored_device) != 0) {
        fprintf(stderr, "restored sync identity mismatch\n");
        return 1;
    }
    free(source_db);
    free(source_device);
    free(restored_db);
    free(restored_device);

    expect(shio_close(source), "backup close(source)");
    expect(shio_close(restored), "backup close(restored)");
    shio_kernel_destroy(source);
    shio_kernel_destroy(restored);
    remove(path);
    remove(backup_path);
    remove(target_path);
    printf("BACKUP_RESTORE_OK\n");
    return 0;
}

static int sync_mode(const char *path)
{
    static const char *PEER_PASSWORD = "android-sync-peer-password";
    const char *options = "{\"create\":true}";
    char peer_path[1024];
    shio_kernel *a = NULL;
    shio_kernel *b = NULL;
    uint8_t *buf = NULL;
    size_t len = 0;
    uint8_t *frame = NULL;
    size_t frame_len = 0;
    uint8_t *json = NULL;
    size_t json_len = 0;
    uint8_t id[16];
    uint8_t peer_id[16];
    uint8_t to[16];
    char id_text[37];
    char *text;
    char *secret;
    char *to_text;
    int32_t status;
    int i;

    snprintf(peer_path, sizeof(peer_path), "%s.peer", path);
    remove(path);
    remove(peer_path);

    expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                     (const uint8_t *)options, strlen(options), &a),
           "sync open a");
    expect(shio_sync_enable(a, NULL, &buf, &len), "sync_enable(a)");
    text = take_text(buf, len, "sync_enable(a)");
    printf("sync a: %s\n", text);
    contains(text, "\"db_id\"", "sync_enable(a)");
    free(text);

    expect(shio_sync_create_pairing(a, 600, &frame, &frame_len, &json, &json_len),
           "create_pairing");
    text = take_text(json, json_len, "create_pairing");
    printf("pairing: %s\n", text);
    contains(text, "\"fingerprint\"", "create_pairing");
    secret = json_string(text, "secret", "create_pairing");
    free(text);
    expect(shio_open_with_pairing(peer_path,
                                  (const uint8_t *)PEER_PASSWORD,
                                  strlen(PEER_PASSWORD),
                                  (const uint8_t *)options, strlen(options),
                                  frame, frame_len,
                                  (const uint8_t *)secret, strlen(secret), &b),
           "open_with_pairing");
    shio_free(frame, frame_len);
    free(secret);

    text = sync_info_text(a, "sync_info(a)");
    {
        char *db_a = json_string(text, "db_id", "sync_info(a)");
        char *db_b = NULL;
        char *info_b = sync_info_text(b, "sync_info(b)");
        db_b = json_string(info_b, "db_id", "sync_info(b)");
        if (strcmp(db_a, db_b) != 0) {
            fprintf(stderr, "paired db_id mismatch: %s != %s\n", db_a, db_b);
            exit(1);
        }
        free(db_a);
        free(db_b);
        free(info_b);
    }
    free(text);

    for (i = 0; i < 3; i++) {
        char payload[64];
        snprintf(payload, sizeof(payload), "{\"name\":\"alpha\",\"n\":%d}", i);
        expect(shio_insert(a, "vault", "entries",
                           (const uint8_t *)payload, strlen(payload),
                           i == 0 ? id : peer_id, &buf, &len),
               "sync insert(a)");
        shio_free(buf, len);
        buf = NULL;
        len = 0;
    }
    pull_changes(a, b, "changes(a->b)");
    expect(shio_get(b, "vault", "entries", id, &buf, &len), "sync get(b)");
    text = take_text(buf, len, "sync get(b)");
    contains(text, "alpha", "sync get(b)");
    free(text);

    expect(shio_insert(b, "vault", "entries",
                       (const uint8_t *)"{\"name\":\"beta\"}",
                       strlen("{\"name\":\"beta\"}"), peer_id, &buf, &len),
           "sync insert(b)");
    shio_free(buf, len);
    buf = NULL;
    len = 0;
    pull_changes(b, a, "changes(b->a)");
    expect(shio_get(a, "vault", "entries", peer_id, &buf, &len),
           "sync get(a)");
    text = take_text(buf, len, "sync get(a)");
    contains(text, "beta", "sync get(a)");
    free(text);

    copy_snapshot(a, b, "snapshot(a->b)");

    expect(shio_sync_begin_rotation(a, &json, &json_len), "begin_rotation");
    text = take_text(json, json_len, "begin_rotation");
    printf("rotation: %s\n", text);
    to_text = json_string(text, "to", "begin_rotation");
    hex_decode_16(to_text, to);
    free(to_text);
    free(text);

    text = migration_progress_text(a, "migration_progress(before)");
    contains(text, "\"active\":false", "migration_progress(before)");
    contains(text, "\"done\":false", "migration_progress(before)");
    free(text);
    expect(shio_sync_cancel_migration(a), "cancel_migration(idle)");

    expect(shio_sync_offers(a, &frame, &frame_len), "sync_offers(a)");
    if (frame == NULL || frame_len == 0) {
        fprintf(stderr, "sync_offers(a) returned an empty frame\n");
        return 1;
    }
    expect(shio_sync_apply_offers(b, frame, frame_len), "apply_offers(b)");
    shio_free(frame, frame_len);
    frame = NULL;

    expect(shio_sync_migrate_all_ex(a, to, 1, 0), "migrate_all_ex(a)");
    text = migration_progress_text(a, "migration_progress(after)");
    contains(text, "\"active\":false", "migration_progress(after)");
    contains(text, "\"done\":true", "migration_progress(after)");
    free(text);
    pull_changes(a, b, "changes(after rotation)");
    expect(shio_get(b, "vault", "entries", id, &buf, &len),
           "sync get(b after rotation)");
    text = take_text(buf, len, "sync get(b after rotation)");
    contains(text, "alpha", "sync get(b after rotation)");
    free(text);
    expect(shio_sync_ack_rotation(b, to), "ack_rotation(b)");

    text = sync_info_text(a, "sync_info(a after rotation)");
    {
        uint64_t log_end = json_u64(text, "log_end", "sync_info(a after rotation)");
        uint64_t removed = 0;
        expect(shio_sync_compact(a, log_end, &removed), "compact(a)");
        printf("compacted entries: %llu\n", (unsigned long long)removed);
    }
    free(text);
    status = shio_sync_changes(a, 0, 2, &frame, &frame_len, NULL, NULL);
    if (status != SHIO_SYNC_RESYNC_REQUIRED) {
        fail(status, "sync_changes(after compact)");
    }
    if (frame != NULL) {
        fprintf(stderr, "sync_changes(after compact) returned a frame\n");
        shio_free(frame, frame_len);
        return 1;
    }
    printf("compacted cursor correctly requires resync\n");

    uuid_text(id, id_text);
    printf("sync record: %s\n", id_text);
    expect(shio_close(b), "sync close b");
    shio_kernel_destroy(b);
    expect(shio_close(a), "sync close a");
    shio_kernel_destroy(a);
    remove(peer_path);
    remove(path);
    printf("ANDROID_SYNC_OK\n");
    return 0;
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
        fprintf(stderr,
                "usage: %s <database-path> "
                "[smoke|sync|hold|probe|check|read-hold|read-probe|read-locked|"
                "rotate-many|resume-rotation|migration-progress|maintenance|"
                "cooperative|compact|backup-restore]\n",
                argv[0]);
        return 2;
    }
    path = argv[1];
    mode = argc == 3 ? argv[2] : "smoke";

    if (strcmp(mode, "read-hold") == 0) {
        expect(shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                         (const uint8_t *)"{\"read_only\":true}",
                         strlen("{\"read_only\":true}"), &kernel),
               "shio_open(read-hold)");
        expect(shio_stats(kernel, &buf, &len), "shio_stats(read-hold)");
        shio_free(buf, len);
        printf("READ_HOLD_READY\n");
        fflush(stdout);
        sleep(20);
        expect(shio_close(kernel), "shio_close(read-hold)");
        shio_kernel_destroy(kernel);
        printf("READ_HOLD_DONE\n");
        return 0;
    }

    if (strcmp(mode, "read-probe") == 0) {
        status = shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                           (const uint8_t *)"{\"read_only\":true}",
                           strlen("{\"read_only\":true}"), &kernel);
        if (status == SHIO_OK) {
            expect(shio_stats(kernel, &buf, &len), "shio_stats(read-probe)");
            shio_free(buf, len);
            expect(shio_close(kernel), "shio_close(read-probe)");
            shio_kernel_destroy(kernel);
            printf("READ_PROBE_OK\n");
            return 0;
        }
        if (status == SHIO_LOCKED) {
            printf("READ_PROBE_LOCKED\n");
            return 1;
        }
        fail(status, "shio_open(read-probe)");
    }

    if (strcmp(mode, "read-locked") == 0) {
        status = shio_open(path, (const uint8_t *)PASSWORD, strlen(PASSWORD),
                           (const uint8_t *)"{\"read_only\":true}",
                           strlen("{\"read_only\":true}"), &kernel);
        if (status == SHIO_LOCKED) {
            printf("READ_LOCKED\n");
            return 0;
        }
        if (status == SHIO_OK) {
            printf("READ_UNEXPECTED_OK\n");
            expect(shio_close(kernel), "shio_close(read-locked)");
            shio_kernel_destroy(kernel);
            return 1;
        }
        fail(status, "shio_open(read-locked)");
    }

    if (strcmp(mode, "sync") == 0) {
        return sync_mode(path);
    }

    if (strcmp(mode, "migration-progress") == 0) {
        return migration_progress_mode(path);
    }

    if (strcmp(mode, "maintenance") == 0) {
        return maintenance_mode(path);
    }

    if (strcmp(mode, "cooperative") == 0) {
        return cooperative_mode(path);
    }

    if (strcmp(mode, "compact") == 0) {
        return compact_mode(path);
    }

    if (strcmp(mode, "backup-restore") == 0) {
        return backup_restore_mode(path);
    }

    if (strcmp(mode, "rotate-many") == 0) {
        return rotate_many(path);
    }

    if (strcmp(mode, "resume-rotation") == 0) {
        return resume_rotation(path);
    }

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
