/*
 * bench-native.c — Native baseline for coconutOS inference benchmark.
 *
 * Identical model (same LCG PRNG seed, same dimensions, same math) as the
 * coconutOS llama-inference shard, running on the host OS. Measures per-token
 * latency for direct comparison.
 *
 * Build:
 *   cc -O2 -o bench-native scripts/bench-native.c -lm
 *
 * Run:
 *   ./bench-native
 */

#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <string.h>
#include <math.h>
#include <time.h>

/* -----------------------------------------------------------------------
 * LCG PRNG — must match build.rs exactly
 * ----------------------------------------------------------------------- */

static uint64_t lcg_state;

static void lcg_seed(uint64_t seed)
{
    lcg_state = seed;
}

static uint64_t lcg_next(void)
{
    lcg_state = lcg_state * 6364136223846793005ULL + 1442695040888963407ULL;
    return lcg_state;
}

static float lcg_f32(void)
{
    uint64_t bits = lcg_next();
    uint32_t u = (uint32_t)(bits >> 32);
    return ((float)u / (float)UINT32_MAX) - 0.5f;
}

/* -----------------------------------------------------------------------
 * Timing
 * ----------------------------------------------------------------------- */

static uint64_t now_ns(void)
{
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
}

/* -----------------------------------------------------------------------
 * Transformer (same math as shards/llama-inference/main.c)
 * ----------------------------------------------------------------------- */

struct config {
    int dim;
    int hidden_dim;
    int n_layers;
    int n_heads;
    int n_kv_heads;
    int vocab_size;
    int seq_len;
};

struct weights {
    float *token_embedding_table;
    float *rms_att_weight;
    float *wq, *wk, *wv, *wo;
    float *rms_ffn_weight;
    float *w1, *w2, *w3;
    float *rms_final_weight;
    float *freq_cis_real, *freq_cis_imag;
    float *wcls;
};

struct run_state {
    float *x, *xb, *xb2, *hb, *hb2;
    float *q, *k, *v, *att, *logits;
    float *key_cache, *value_cache;
};

static void rmsnorm(float *o, float *x, float *w, int size)
{
    float ss = 0.0f;
    for (int i = 0; i < size; i++)
        ss += x[i] * x[i];
    ss = 1.0f / sqrtf(ss / (float)size + 1e-5f);
    for (int i = 0; i < size; i++)
        o[i] = w[i] * (ss * x[i]);
}

static void softmax(float *x, int size)
{
    float max_val = x[0];
    for (int i = 1; i < size; i++)
        if (x[i] > max_val) max_val = x[i];
    float sum = 0.0f;
    for (int i = 0; i < size; i++) {
        x[i] = expf(x[i] - max_val);
        sum += x[i];
    }
    for (int i = 0; i < size; i++)
        x[i] /= sum;
}

static void matmul(float *xout, float *x, float *w, int n, int d)
{
    for (int i = 0; i < d; i++) {
        float val = 0.0f;
        for (int j = 0; j < n; j++)
            val += w[i * n + j] * x[j];
        xout[i] = val;
    }
}

static int argmax(float *v, int n)
{
    int max_i = 0;
    float max_val = v[0];
    for (int i = 1; i < n; i++) {
        if (v[i] > max_val) {
            max_val = v[i];
            max_i = i;
        }
    }
    return max_i;
}

static void forward(struct config *cfg, struct weights *w, struct run_state *s,
                    int token, int pos)
{
    int dim = cfg->dim;
    int hidden_dim = cfg->hidden_dim;
    int head_size = dim / cfg->n_heads;
    int kv_dim = cfg->n_kv_heads * head_size;
    int kv_mul = cfg->n_heads / cfg->n_kv_heads;

    float *embed = w->token_embedding_table + token * dim;
    for (int i = 0; i < dim; i++)
        s->x[i] = embed[i];

    for (int l = 0; l < cfg->n_layers; l++) {
        rmsnorm(s->xb, s->x, w->rms_att_weight + l * dim, dim);

        matmul(s->q, s->xb, w->wq + l * dim * dim, dim, dim);
        matmul(s->k, s->xb, w->wk + l * dim * kv_dim, dim, kv_dim);
        matmul(s->v, s->xb, w->wv + l * dim * kv_dim, dim, kv_dim);

        for (int i = 0; i < dim; i += 2) {
            int head_dim = i % head_size;
            float fcr = w->freq_cis_real[pos * (head_size / 2) + head_dim / 2];
            float fci = w->freq_cis_imag[pos * (head_size / 2) + head_dim / 2];
            float v0 = s->q[i];
            float v1 = s->q[i + 1];
            s->q[i]     = v0 * fcr - v1 * fci;
            s->q[i + 1] = v0 * fci + v1 * fcr;
        }
        for (int i = 0; i < kv_dim; i += 2) {
            int head_dim = i % head_size;
            float fcr = w->freq_cis_real[pos * (head_size / 2) + head_dim / 2];
            float fci = w->freq_cis_imag[pos * (head_size / 2) + head_dim / 2];
            float v0 = s->k[i];
            float v1 = s->k[i + 1];
            s->k[i]     = v0 * fcr - v1 * fci;
            s->k[i + 1] = v0 * fci + v1 * fcr;
        }

        int loff = l * cfg->seq_len * kv_dim;
        float *key_cache_row   = s->key_cache   + loff + pos * kv_dim;
        float *value_cache_row = s->value_cache + loff + pos * kv_dim;
        for (int i = 0; i < kv_dim; i++) {
            key_cache_row[i]   = s->k[i];
            value_cache_row[i] = s->v[i];
        }

        for (int h = 0; h < cfg->n_heads; h++) {
            float *q_h = s->q + h * head_size;
            float *att_h = s->att + h * cfg->seq_len;
            int kv_h = h / kv_mul;

            for (int t = 0; t <= pos; t++) {
                float *k_t = s->key_cache + loff + t * kv_dim + kv_h * head_size;
                float score = 0.0f;
                for (int i = 0; i < head_size; i++)
                    score += q_h[i] * k_t[i];
                score /= sqrtf((float)head_size);
                att_h[t] = score;
            }

            softmax(att_h, pos + 1);

            float *xb_h = s->xb + h * head_size;
            for (int i = 0; i < head_size; i++) xb_h[i] = 0.0f;
            for (int t = 0; t <= pos; t++) {
                float *v_t = s->value_cache + loff + t * kv_dim + kv_h * head_size;
                float a = att_h[t];
                for (int i = 0; i < head_size; i++)
                    xb_h[i] += a * v_t[i];
            }
        }

        matmul(s->xb2, s->xb, w->wo + l * dim * dim, dim, dim);
        for (int i = 0; i < dim; i++)
            s->x[i] += s->xb2[i];

        rmsnorm(s->xb, s->x, w->rms_ffn_weight + l * dim, dim);

        matmul(s->hb, s->xb, w->w1 + l * dim * hidden_dim, dim, hidden_dim);
        matmul(s->hb2, s->xb, w->w3 + l * dim * hidden_dim, dim, hidden_dim);

        for (int i = 0; i < hidden_dim; i++) {
            float val = s->hb[i];
            val = val * (1.0f / (1.0f + expf(-val)));
            val = val * s->hb2[i];
            s->hb[i] = val;
        }

        matmul(s->xb, s->hb, w->w2 + l * hidden_dim * dim, hidden_dim, dim);
        for (int i = 0; i < dim; i++)
            s->x[i] += s->xb[i];
    }

    rmsnorm(s->x, s->x, w->rms_final_weight, dim);
    matmul(s->logits, s->x, w->wcls, dim, cfg->vocab_size);
}

/* -----------------------------------------------------------------------
 * Model generation — must match build.rs LCG sequence exactly
 * ----------------------------------------------------------------------- */

static float *gen_floats(int count)
{
    float *buf = malloc(count * sizeof(float));
    for (int i = 0; i < count; i++)
        buf[i] = lcg_f32();
    return buf;
}

int main(void)
{
    struct config cfg = {
        .dim = 32,
        .hidden_dim = 64,
        .n_layers = 2,
        .n_heads = 4,
        .n_kv_heads = 4,
        .vocab_size = 32,
        .seq_len = 32,
    };
    int head_size = cfg.dim / cfg.n_heads;
    int kv_dim = cfg.n_kv_heads * head_size;

    /* Generate weights with same LCG as build.rs */
    lcg_seed(42);

    struct weights w;
    w.token_embedding_table = gen_floats(cfg.vocab_size * cfg.dim);
    w.rms_att_weight = gen_floats(cfg.n_layers * cfg.dim);
    w.wq = gen_floats(cfg.n_layers * cfg.dim * cfg.dim);
    w.wk = gen_floats(cfg.n_layers * cfg.dim * kv_dim);
    w.wv = gen_floats(cfg.n_layers * cfg.dim * kv_dim);
    w.wo = gen_floats(cfg.n_layers * cfg.dim * cfg.dim);
    w.rms_ffn_weight = gen_floats(cfg.n_layers * cfg.dim);
    w.w1 = gen_floats(cfg.n_layers * cfg.dim * cfg.hidden_dim);
    w.w2 = gen_floats(cfg.n_layers * cfg.hidden_dim * cfg.dim);
    w.w3 = gen_floats(cfg.n_layers * cfg.dim * cfg.hidden_dim);
    w.rms_final_weight = gen_floats(cfg.dim);
    w.freq_cis_real = gen_floats(cfg.seq_len * head_size / 2);
    w.freq_cis_imag = gen_floats(cfg.seq_len * head_size / 2);
    w.wcls = w.token_embedding_table; /* shared weights */

    /* Allocate run state */
    struct run_state s;
    s.x      = calloc(cfg.dim, sizeof(float));
    s.xb     = calloc(cfg.dim, sizeof(float));
    s.xb2    = calloc(cfg.dim, sizeof(float));
    s.hb     = calloc(cfg.hidden_dim, sizeof(float));
    s.hb2    = calloc(cfg.hidden_dim, sizeof(float));
    s.q      = calloc(cfg.dim, sizeof(float));
    s.k      = calloc(kv_dim, sizeof(float));
    s.v      = calloc(kv_dim, sizeof(float));
    s.att    = calloc(cfg.n_heads * cfg.seq_len, sizeof(float));
    s.logits = calloc(cfg.vocab_size, sizeof(float));
    s.key_cache   = calloc(cfg.n_layers * cfg.seq_len * kv_dim, sizeof(float));
    s.value_cache = calloc(cfg.n_layers * cfg.seq_len * kv_dim, sizeof(float));

    static const char VOCAB[32] = {
        'a','b','c','d','e','f','g','h','i','j','k','l','m',
        'n','o','p','q','r','s','t','u','v','w','x','y','z',
        ' ','\n','.',',','!','?'
    };

    int n_tokens = 16;
    int token = 0;
    uint64_t token_ns[16];

    uint64_t bench_start = now_ns();

    for (int pos = 0; pos < n_tokens; pos++) {
        uint64_t t0 = now_ns();
        forward(&cfg, &w, &s, token, pos);
        int next = argmax(s.logits, cfg.vocab_size);
        uint64_t t1 = now_ns();

        token_ns[pos] = t1 - t0;
        char ch = (next >= 0 && next < 32) ? VOCAB[next] : '?';
        printf("token %d -> '%c'\n", pos, ch);

        token = next;
    }

    uint64_t bench_end = now_ns();
    uint64_t total_ns = bench_end - bench_start;

    /* Output in same format as coconut-prof.py expects */
    printf("\n--- Inference Benchmark (native) ---\n");
    printf("bench: total_tokens=%d total_ns=%llu dim=%d layers=%d\n",
           n_tokens, (unsigned long long)total_ns, cfg.dim, cfg.n_layers);

    for (int i = 0; i < n_tokens; i++) {
        printf("bench: token=%d ns=%llu\n", i, (unsigned long long)token_ns[i]);
    }

    double total_ms = total_ns / 1e6;
    double tok_per_sec = n_tokens / (total_ns / 1e9);
    printf("\nSummary: %.2f ms total, %.1f tokens/sec\n", total_ms, tok_per_sec);

    return 0;
}
