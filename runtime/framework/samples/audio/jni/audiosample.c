// The native half of AudioActivity, built by the NDK: plays and records through OpenSL ES and AAudio
// the way apps and libraries such as oboe do, and reports what it saw. audio_smoke.sh checks the
// reports against the samples the device received and the samples it supplied.
#include <SLES/OpenSLES.h>
#include <SLES/OpenSLES_Android.h>
#include <aaudio/AAudio.h>
#include <jni.h>
#include <pthread.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

// OpenSL ES is deprecated since Android 30 in favor of AAudio, and still used by apps such as RustDesk.
#pragma clang diagnostic ignored "-Wdeprecated-declarations"

static char report[512];

static short saw(int i, int hz, int rate) {
    return (short)((((long long)i * hz * 65536 / rate) & 0xFFFF) - 32768);
}

static long long now_ns(void) {
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    return now.tv_sec * 1000000000LL + now.tv_nsec;
}

// OpenSL ES: a stereo tone in 10 ms buffers through a two-buffer queue, refilled from its callback.
#define SL_RATE 44100
#define SL_CHUNK 441

typedef struct {
    short buffers[2][SL_CHUNK * 2];
    int queued, total, completed;
    pthread_mutex_t lock;
    pthread_cond_t done;
} Player;

static void enqueue_next(Player* p, SLAndroidSimpleBufferQueueItf queue) {
    short* buffer = p->buffers[(p->queued / SL_CHUNK) % 2];
    for (int f = 0; f < SL_CHUNK; f++) {
        buffer[2 * f] = saw(p->queued + f, 330, SL_RATE);
        buffer[2 * f + 1] = saw(p->queued + f, 660, SL_RATE);
    }
    p->queued += SL_CHUNK;
    (*queue)->Enqueue(queue, buffer, sizeof(p->buffers[0]));
}

static void on_played(SLAndroidSimpleBufferQueueItf queue, void* context) {
    Player* p = context;
    pthread_mutex_lock(&p->lock);
    p->completed++;
    if (p->queued < p->total) enqueue_next(p, queue);
    if (p->completed * SL_CHUNK >= p->total) pthread_cond_signal(&p->done);
    pthread_mutex_unlock(&p->lock);
}

typedef struct {
    short samples[1600];
    int filled;
    pthread_mutex_t lock;
    pthread_cond_t done;
} Recorder;

static void on_recorded(SLAndroidSimpleBufferQueueItf queue, void* context) {
    (void)queue;
    Recorder* r = context;
    pthread_mutex_lock(&r->lock);
    r->filled = 1;
    pthread_cond_signal(&r->done);
    pthread_mutex_unlock(&r->lock);
}

static int wait_for(pthread_mutex_t* lock, pthread_cond_t* cond, int* flag_or_null, int (*ready)(void*), void* arg) {
    struct timespec deadline;
    clock_gettime(CLOCK_REALTIME, &deadline);
    deadline.tv_sec += 3;
    pthread_mutex_lock(lock);
    int ok = 1;
    while (flag_or_null ? !*flag_or_null : !ready(arg)) {
        if (pthread_cond_timedwait(cond, lock, &deadline) != 0) {
            ok = 0;
            break;
        }
    }
    pthread_mutex_unlock(lock);
    return ok;
}

static int player_done(void* arg) {
    Player* p = arg;
    return p->completed * SL_CHUNK >= p->total;
}

#define CHECK(call, what)                                                         \
    do {                                                                          \
        SLresult result_ = (call);                                                \
        if (result_ != SL_RESULT_SUCCESS) {                                       \
            snprintf(report, sizeof(report), "%s failed: %u", what, (unsigned)result_); \
            return report;                                                        \
        }                                                                         \
    } while (0)

static const char* open_sl(int frames) {
    SLObjectItf engine_object, mix, player_object, recorder_object;
    SLEngineItf engine;
    CHECK(slCreateEngine(&engine_object, 0, NULL, 0, NULL, NULL), "slCreateEngine");
    CHECK((*engine_object)->Realize(engine_object, SL_BOOLEAN_FALSE), "engine Realize");
    CHECK((*engine_object)->GetInterface(engine_object, SL_IID_ENGINE, &engine), "SL_IID_ENGINE");
    CHECK((*engine)->CreateOutputMix(engine, &mix, 0, NULL, NULL), "CreateOutputMix");
    CHECK((*mix)->Realize(mix, SL_BOOLEAN_FALSE), "mix Realize");

    SLDataLocator_AndroidSimpleBufferQueue queue_locator = {SL_DATALOCATOR_ANDROIDSIMPLEBUFFERQUEUE, 2};
    SLDataFormat_PCM pcm = {SL_DATAFORMAT_PCM, 2, SL_SAMPLINGRATE_44_1, SL_PCMSAMPLEFORMAT_FIXED_16,
                            SL_PCMSAMPLEFORMAT_FIXED_16, SL_SPEAKER_FRONT_LEFT | SL_SPEAKER_FRONT_RIGHT,
                            SL_BYTEORDER_LITTLEENDIAN};
    SLDataSource source = {&queue_locator, &pcm};
    SLDataLocator_OutputMix mix_locator = {SL_DATALOCATOR_OUTPUTMIX, mix};
    SLDataSink sink = {&mix_locator, NULL};
    const SLInterfaceID ids[2] = {SL_IID_BUFFERQUEUE, SL_IID_VOLUME};
    const SLboolean required[2] = {SL_BOOLEAN_TRUE, SL_BOOLEAN_TRUE};
    CHECK((*engine)->CreateAudioPlayer(engine, &player_object, &source, &sink, 2, ids, required),
          "CreateAudioPlayer");
    CHECK((*player_object)->Realize(player_object, SL_BOOLEAN_FALSE), "player Realize");
    SLPlayItf play;
    SLAndroidSimpleBufferQueueItf queue;
    SLVolumeItf volume;
    CHECK((*player_object)->GetInterface(player_object, SL_IID_PLAY, &play), "SL_IID_PLAY");
    CHECK((*player_object)->GetInterface(player_object, SL_IID_BUFFERQUEUE, &queue), "SL_IID_BUFFERQUEUE");
    CHECK((*player_object)->GetInterface(player_object, SL_IID_VOLUME, &volume), "SL_IID_VOLUME");
    SLmillibel max_volume = -1;
    CHECK((*volume)->GetMaxVolumeLevel(volume, &max_volume), "GetMaxVolumeLevel");

    static Player player;
    memset(&player, 0, sizeof(player));
    pthread_mutex_init(&player.lock, NULL);
    pthread_cond_init(&player.done, NULL);
    player.total = frames * SL_RATE / 48000 / SL_CHUNK * SL_CHUNK;
    CHECK((*queue)->RegisterCallback(queue, on_played, &player), "RegisterCallback");
    pthread_mutex_lock(&player.lock);
    enqueue_next(&player, queue);
    enqueue_next(&player, queue);
    pthread_mutex_unlock(&player.lock);
    CHECK((*play)->SetPlayState(play, SL_PLAYSTATE_PLAYING), "SetPlayState");
    int played = wait_for(&player.lock, &player.done, NULL, player_done, &player);
    SLmillisecond position = 0;
    (*play)->GetPosition(play, &position);
    SLAndroidSimpleBufferQueueState state;
    (*queue)->GetState(queue, &state);
    (*play)->SetPlayState(play, SL_PLAYSTATE_STOPPED);
    (*player_object)->Destroy(player_object);
    if (!played) return "the player's buffers did not all play";

    SLDataLocator_IODevice device = {SL_DATALOCATOR_IODEVICE, SL_IODEVICE_AUDIOINPUT,
                                     SL_DEFAULTDEVICEID_AUDIOINPUT, NULL};
    SLDataSource mic = {&device, NULL};
    SLDataFormat_PCM mono = {SL_DATAFORMAT_PCM, 1, SL_SAMPLINGRATE_16, SL_PCMSAMPLEFORMAT_FIXED_16,
                             SL_PCMSAMPLEFORMAT_FIXED_16, SL_SPEAKER_FRONT_CENTER, SL_BYTEORDER_LITTLEENDIAN};
    SLDataSink into = {&queue_locator, &mono};
    const SLInterfaceID record_ids[2] = {SL_IID_ANDROIDSIMPLEBUFFERQUEUE, SL_IID_ANDROIDCONFIGURATION};
    const SLboolean record_required[2] = {SL_BOOLEAN_TRUE, SL_BOOLEAN_FALSE};
    CHECK((*engine)->CreateAudioRecorder(engine, &recorder_object, &mic, &into, 2, record_ids, record_required),
          "CreateAudioRecorder");
    CHECK((*recorder_object)->Realize(recorder_object, SL_BOOLEAN_FALSE), "recorder Realize");
    SLRecordItf record;
    SLAndroidSimpleBufferQueueItf record_queue;
    CHECK((*recorder_object)->GetInterface(recorder_object, SL_IID_RECORD, &record), "SL_IID_RECORD");
    CHECK((*recorder_object)->GetInterface(recorder_object, SL_IID_ANDROIDSIMPLEBUFFERQUEUE, &record_queue),
          "record queue");
    static Recorder recorder;
    memset(&recorder, 0, sizeof(recorder));
    pthread_mutex_init(&recorder.lock, NULL);
    pthread_cond_init(&recorder.done, NULL);
    CHECK((*record_queue)->RegisterCallback(record_queue, on_recorded, &recorder), "record RegisterCallback");
    CHECK((*record_queue)->Enqueue(record_queue, recorder.samples, sizeof(recorder.samples)), "record Enqueue");
    CHECK((*record)->SetRecordState(record, SL_RECORDSTATE_RECORDING), "SetRecordState");
    int recorded = wait_for(&recorder.lock, &recorder.done, &recorder.filled, NULL, NULL);
    (*record)->SetRecordState(record, SL_RECORDSTATE_STOPPED);
    (*recorder_object)->Destroy(recorder_object);
    (*mix)->Destroy(mix);
    (*engine_object)->Destroy(engine_object);
    if (!recorded) return "the recorder's buffer did not fill";
    long long sum = 0;
    for (int i = 0; i < 1600; i++) sum += recorder.samples[i];
    snprintf(report, sizeof(report),
             "played %d frames in %d buffers, position %u ms, queue %u, max volume %d; recorded sum %lld, first %d %d %d",
             player.total, player.completed, (unsigned)position, (unsigned)state.count, max_volume, sum,
             recorder.samples[0], recorder.samples[1], recorder.samples[2]);
    return report;
}

// AAudio: a float callback stream, as oboe opens one, then a blocking 16-bit input stream.
typedef struct {
    int frame, total, callbacks;
    long long first_ns, last_ns;
} Tone;

static aaudio_data_callback_result_t on_audio(AAudioStream* stream, void* user, void* audio, int32_t frames) {
    (void)stream;
    Tone* tone = user;
    float* out = audio;
    for (int f = 0; f < frames; f++) {
        int at = tone->frame + f;
        float value = at < tone->total ? (float)(at % 64) / 64.0f - 0.5f : 0.0f;
        out[2 * f] = value;
        out[2 * f + 1] = -value;
    }
    tone->frame += frames;
    long long now = now_ns();
    if (tone->callbacks++ == 0) tone->first_ns = now;
    tone->last_ns = now;
    return tone->frame >= tone->total ? AAUDIO_CALLBACK_RESULT_STOP : AAUDIO_CALLBACK_RESULT_CONTINUE;
}

#define ACHECK(call, what)                                                                         \
    do {                                                                                           \
        aaudio_result_t result_ = (call);                                                          \
        if (result_ < 0) {                                                                         \
            snprintf(report, sizeof(report), "%s failed: %s", what, AAudio_convertResultToText(result_)); \
            return report;                                                                         \
        }                                                                                          \
    } while (0)

static const char* run_aaudio(int frames) {
    AAudioStreamBuilder* builder;
    AAudioStream* stream;
    static Tone tone;
    memset(&tone, 0, sizeof(tone));
    tone.total = frames * 32000 / 48000;
    ACHECK(AAudio_createStreamBuilder(&builder), "AAudio_createStreamBuilder");
    AAudioStreamBuilder_setDirection(builder, AAUDIO_DIRECTION_OUTPUT);
    AAudioStreamBuilder_setSampleRate(builder, 32000);
    AAudioStreamBuilder_setChannelCount(builder, 2);
    AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_FLOAT);
    AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_LOW_LATENCY);
    AAudioStreamBuilder_setSharingMode(builder, AAUDIO_SHARING_MODE_SHARED);
    AAudioStreamBuilder_setDataCallback(builder, on_audio, &tone);
    ACHECK(AAudioStreamBuilder_openStream(builder, &stream), "openStream");
    int burst = AAudioStream_getFramesPerBurst(stream);
    int rate = AAudioStream_getSampleRate(stream);
    long long started = now_ns();
    ACHECK(AAudioStream_requestStart(stream), "requestStart");
    aaudio_stream_state_t state = AAudioStream_getState(stream);
    for (int i = 0; i < 300 && state != AAUDIO_STREAM_STATE_STOPPED; i++) {
        AAudioStream_waitForStateChange(stream, state, &state, 10000000LL);
    }
    long long written = AAudioStream_getFramesWritten(stream);
    int xruns = AAudioStream_getXRunCount(stream);
    AAudioStream_close(stream);
    if (state != AAUDIO_STREAM_STATE_STOPPED) {
        snprintf(report, sizeof(report), "the callback stream did not stop: %s", AAudio_convertStreamStateToText(state));
        return report;
    }
    long long first_us = (tone.first_ns - started) / 1000;
    long long period_us = tone.callbacks > 1 ? (tone.last_ns - tone.first_ns) / 1000 / (tone.callbacks - 1) : 0;

    AAudioStreamBuilder_setDirection(builder, AAUDIO_DIRECTION_INPUT);
    AAudioStreamBuilder_setSampleRate(builder, 16000);
    AAudioStreamBuilder_setChannelCount(builder, 1);
    AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setDataCallback(builder, NULL, NULL);
    ACHECK(AAudioStreamBuilder_openStream(builder, &stream), "input openStream");
    AAudioStreamBuilder_delete(builder);
    ACHECK(AAudioStream_requestStart(stream), "input requestStart");
    static short samples[1600];
    int read = 0;
    while (read < 1600) {
        aaudio_result_t got = AAudioStream_read(stream, samples + read, 1600 - read, 1000000000LL);
        if (got <= 0) break;
        read += got;
    }
    AAudioStream_requestStop(stream);
    AAudioStream_close(stream);
    long long sum = 0;
    for (int i = 0; i < read; i++) sum += samples[i];
    snprintf(report, sizeof(report),
             "rate %d, burst %d, callbacks %d, written %lld, xruns %d, first callback %lld us, period %lld us; "
             "read %d, sum %lld, first %d %d %d",
             rate, burst, tone.callbacks, written, xruns, first_us, period_us, read, sum, samples[0], samples[1],
             samples[2]);
    return report;
}

JNIEXPORT jstring JNICALL Java_com_example_audio_AudioActivity_openSl(JNIEnv* env, jclass c, jint frames) {
    (void)c;
    return (*env)->NewStringUTF(env, open_sl(frames));
}

JNIEXPORT jstring JNICALL Java_com_example_audio_AudioActivity_aaudio(JNIEnv* env, jclass c, jint frames) {
    (void)c;
    return (*env)->NewStringUTF(env, run_aaudio(frames));
}
