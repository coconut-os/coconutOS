/*
 * llama-inference — proof-of-concept transformer inference shard.
 *
 * Stripped-down port of Karpathy's llama2.c for coconutOS. Demonstrates
 * end-to-end model loading from ramdisk, heap allocation via SYS_MMAP,
 * and multi-layer transformer forward pass with float math.
 *
 * Model format: 7×i32 header + flat f32 weight arrays (llama2.c checkpoint).
 * Config: dim=32, hidden_dim=64, n_layers=2, n_heads=4, vocab=32.
 *
 * All mutable state lives on the stack (not BSS) because shard code pages
 * are mapped R+X. The heap allocator state is passed by pointer.
 */

#include "../../include/coconut.h"

/* clang -ffreestanding may emit implicit calls to memset/memcpy */

void *memset(void *s, int c, size_t n)
{
    uint8_t *p = (uint8_t *)s;
    while (n--)
        *p++ = (uint8_t)c;
    return s;
}

void *memcpy(void *dest, const void *src, size_t n)
{
    uint8_t *d = (uint8_t *)dest;
    const uint8_t *s = (const uint8_t *)src;
    while (n--)
        *d++ = *s++;
    return dest;
}

/* -----------------------------------------------------------------------
 * Math primitives — no libc, minimal SSE2
 * ----------------------------------------------------------------------- */

static float my_sqrtf(float x)
{
    float result;
    __asm__("sqrtss %1, %0" : "=x"(result) : "x"(x));
    return result;
}

/* Polynomial approximation for expf. Good enough for softmax. */
static float my_expf(float x)
{
    /* Clamp to avoid overflow/underflow */
    if (x > 88.0f) x = 88.0f;
    if (x < -88.0f) return 0.0f;

    /* exp(x) = 2^(x / ln2). Use integer + fractional decomposition. */
    float t = x * 1.4426950408889634f; /* x / ln(2) */
    int i = (int)t;
    if (t < 0.0f && t != (float)i) i--; /* floor */
    float f = t - (float)i;

    /* Polynomial approx of 2^f for f in [0,1): Horner form */
    float p = 1.0f + f * (0.6931472f + f * (0.2402265f + f * (0.0554934f + f * 0.0096940f)));

    /* Multiply by 2^i via bit manipulation of IEEE 754 float */
    union { float fl; uint32_t u; } scale;
    scale.u = (uint32_t)((i + 127) & 0xFF) << 23;

    return p * scale.fl;
}

/* -----------------------------------------------------------------------
 * Print helpers
 * ----------------------------------------------------------------------- */

static void print_u64(uint64_t v)
{
    char buf[20];
    int len = 0;
    if (v == 0) {
        buf[0] = '0';
        len = 1;
    } else {
        while (v > 0) {
            buf[len++] = '0' + (v % 10);
            v /= 10;
        }
        /* Reverse */
        for (int i = 0; i < len / 2; i++) {
            char tmp = buf[i];
            buf[i] = buf[len - 1 - i];
            buf[len - 1 - i] = tmp;
        }
    }
    coconut_serial_write(buf, len);
}

/* -----------------------------------------------------------------------
 * Bump allocator on mmap'd heap — state on stack, not BSS
 * ----------------------------------------------------------------------- */

#define HEAP_VA   0x100000
#define HEAP_PAGES 64 /* 256 KiB */

struct heap {
    uint8_t *ptr;
    uint8_t *end;
};

static void *bump_alloc(struct heap *h, size_t size)
{
    /* Align to 4 bytes for float arrays */
    size_t aligned = (size + 3) & ~(size_t)3;
    if (h->ptr + aligned > h->end)
        return (void *)0;
    void *p = h->ptr;
    h->ptr += aligned;
    return p;
}

/* -----------------------------------------------------------------------
 * Transformer data structures
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
    float *wq;
    float *wk;
    float *wv;
    float *wo;
    float *rms_ffn_weight;
    float *w1;
    float *w2;
    float *w3;
    float *rms_final_weight;
    float *freq_cis_real;
    float *freq_cis_imag;
    float *wcls;
};

struct run_state {
    float *x;
    float *xb;
    float *xb2;
    float *hb;
    float *hb2;
    float *q;
    float *k;
    float *v;
    float *att;
    float *logits;
    float *key_cache;
    float *value_cache;
};

/* -----------------------------------------------------------------------
 * Transformer operations
 * ----------------------------------------------------------------------- */

static void rmsnorm(float *o, float *x, float *w, int size)
{
    float ss = 0.0f;
    for (int i = 0; i < size; i++)
        ss += x[i] * x[i];
    ss = 1.0f / my_sqrtf(ss / (float)size + 1e-5f);
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
        x[i] = my_expf(x[i] - max_val);
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

/* Full transformer forward pass for one token at one position */
static void forward(struct config *cfg, struct weights *w, struct run_state *s,
                    int token, int pos)
{
    int dim = cfg->dim;
    int hidden_dim = cfg->hidden_dim;
    int head_size = dim / cfg->n_heads;
    int kv_dim = cfg->n_kv_heads * head_size;
    int kv_mul = cfg->n_heads / cfg->n_kv_heads;

    /* Copy token embedding into x */
    float *embed = w->token_embedding_table + token * dim;
    for (int i = 0; i < dim; i++)
        s->x[i] = embed[i];

    /* Forward through each layer */
    for (int l = 0; l < cfg->n_layers; l++) {
        /* Attention RMSNorm */
        rmsnorm(s->xb, s->x, w->rms_att_weight + l * dim, dim);

        /* QKV projections */
        matmul(s->q, s->xb, w->wq + l * dim * dim, dim, dim);
        matmul(s->k, s->xb, w->wk + l * dim * kv_dim, dim, kv_dim);
        matmul(s->v, s->xb, w->wv + l * dim * kv_dim, dim, kv_dim);

        /* RoPE relative positional encoding */
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

        /* Cache current K and V */
        int loff = l * cfg->seq_len * kv_dim;
        float *key_cache_row   = s->key_cache   + loff + pos * kv_dim;
        float *value_cache_row = s->value_cache + loff + pos * kv_dim;
        for (int i = 0; i < kv_dim; i++) {
            key_cache_row[i]   = s->k[i];
            value_cache_row[i] = s->v[i];
        }

        /* Multi-head attention */
        for (int h = 0; h < cfg->n_heads; h++) {
            float *q_h = s->q + h * head_size;
            float *att_h = s->att + h * cfg->seq_len;
            int kv_h = h / kv_mul;

            /* Attention scores: dot(q, k) / sqrt(head_size) */
            for (int t = 0; t <= pos; t++) {
                float *k_t = s->key_cache + loff + t * kv_dim + kv_h * head_size;
                float score = 0.0f;
                for (int i = 0; i < head_size; i++)
                    score += q_h[i] * k_t[i];
                score /= my_sqrtf((float)head_size);
                att_h[t] = score;
            }

            softmax(att_h, pos + 1);

            /* Weighted sum of values */
            float *xb_h = s->xb + h * head_size;
            for (int i = 0; i < head_size; i++) xb_h[i] = 0.0f;
            for (int t = 0; t <= pos; t++) {
                float *v_t = s->value_cache + loff + t * kv_dim + kv_h * head_size;
                float a = att_h[t];
                for (int i = 0; i < head_size; i++)
                    xb_h[i] += a * v_t[i];
            }
        }

        /* Output projection + residual */
        matmul(s->xb2, s->xb, w->wo + l * dim * dim, dim, dim);
        for (int i = 0; i < dim; i++)
            s->x[i] += s->xb2[i];

        /* FFN: RMSNorm -> W1/W3 (SiLU gated) -> W2 -> residual */
        rmsnorm(s->xb, s->x, w->rms_ffn_weight + l * dim, dim);

        matmul(s->hb, s->xb, w->w1 + l * dim * hidden_dim, dim, hidden_dim);
        matmul(s->hb2, s->xb, w->w3 + l * dim * hidden_dim, dim, hidden_dim);

        /* SiLU activation: silu(x) = x * sigmoid(x), then gate with w3 */
        for (int i = 0; i < hidden_dim; i++) {
            float val = s->hb[i];
            val = val * (1.0f / (1.0f + my_expf(-val))); /* SiLU */
            val = val * s->hb2[i]; /* gating */
            s->hb[i] = val;
        }

        matmul(s->xb, s->hb, w->w2 + l * hidden_dim * dim, hidden_dim, dim);
        for (int i = 0; i < dim; i++)
            s->x[i] += s->xb[i];
    }

    /* Final RMSNorm */
    rmsnorm(s->x, s->x, w->rms_final_weight, dim);

    /* Classifier: x -> logits */
    matmul(s->logits, s->x, w->wcls, dim, cfg->vocab_size);
}

/* -----------------------------------------------------------------------
 * Token vocabulary (32 printable characters)
 * ----------------------------------------------------------------------- */

static const char VOCAB[32] = {
    'a','b','c','d','e','f','g','h','i','j','k','l','m',
    'n','o','p','q','r','s','t','u','v','w','x','y','z',
    ' ','\n','.',',','!','?'
};

/* -----------------------------------------------------------------------
 * main
 * ----------------------------------------------------------------------- */

int main(void)
{
    coconut_puts("llama-inference: starting\n");

    /* 1. Allocate heap via SYS_MMAP */
    if (coconut_mmap(HEAP_VA, HEAP_PAGES) != 0) {
        coconut_puts("llama-inference: mmap failed\n");
        return 1;
    }
    struct heap h;
    h.ptr = (uint8_t *)HEAP_VA;
    h.end = h.ptr + HEAP_PAGES * 4096;

    /* 2. Open and read model.bin from ramdisk */
    uint64_t fd = coconut_fs_open("/model.bin", 10);
    if (fd == COCONUT_ERROR) {
        coconut_puts("llama-inference: failed to open /model.bin\n");
        return 1;
    }

    uint64_t file_size = coconut_fs_stat(fd);
    if (file_size == COCONUT_ERROR || file_size == 0) {
        coconut_puts("llama-inference: failed to stat model\n");
        return 1;
    }

    /* Allocate space for the model file on heap */
    uint8_t *model_data = (uint8_t *)bump_alloc(&h, file_size);
    if (!model_data) {
        coconut_puts("llama-inference: heap OOM for model\n");
        return 1;
    }

    /* Read in 4 KiB chunks (syscall buffer limit) */
    uint64_t total_read = 0;
    while (total_read < file_size) {
        uint64_t chunk = file_size - total_read;
        if (chunk > 4096) chunk = 4096;
        uint64_t bytes = coconut_fs_read(fd, model_data + total_read, chunk);
        if (bytes == COCONUT_ERROR || bytes == 0)
            break;
        total_read += bytes;
    }
    coconut_fs_close(fd);

    if (total_read < 28) {
        coconut_puts("llama-inference: model file too small\n");
        return 1;
    }

    /* 3. Parse config from header */
    struct config cfg;
    {
        int *hdr = (int *)model_data;
        cfg.dim = hdr[0];
        cfg.hidden_dim = hdr[1];
        cfg.n_layers = hdr[2];
        cfg.n_heads = hdr[3];
        cfg.n_kv_heads = hdr[4];
        cfg.vocab_size = hdr[5];
        cfg.seq_len = hdr[6];
    }
    int shared_weights = cfg.vocab_size > 0;
    if (cfg.vocab_size < 0) cfg.vocab_size = -cfg.vocab_size;

    coconut_puts("llama-inference: loaded model (dim=");
    print_u64(cfg.dim);
    coconut_puts(", layers=");
    print_u64(cfg.n_layers);
    coconut_puts(", vocab=");
    print_u64(cfg.vocab_size);
    coconut_puts(")\n");

    /* 4. Map weight pointers into model data */
    struct weights w;
    int head_size = cfg.dim / cfg.n_heads;
    int kv_dim = cfg.n_kv_heads * head_size;
    float *ptr = (float *)(model_data + 7 * 4);

    w.token_embedding_table = ptr; ptr += cfg.vocab_size * cfg.dim;
    w.rms_att_weight = ptr;        ptr += cfg.n_layers * cfg.dim;
    w.wq = ptr;                    ptr += cfg.n_layers * cfg.dim * (cfg.n_heads * head_size);
    w.wk = ptr;                    ptr += cfg.n_layers * cfg.dim * kv_dim;
    w.wv = ptr;                    ptr += cfg.n_layers * cfg.dim * kv_dim;
    w.wo = ptr;                    ptr += cfg.n_layers * (cfg.n_heads * head_size) * cfg.dim;
    w.rms_ffn_weight = ptr;        ptr += cfg.n_layers * cfg.dim;
    w.w1 = ptr;                    ptr += cfg.n_layers * cfg.dim * cfg.hidden_dim;
    w.w2 = ptr;                    ptr += cfg.n_layers * cfg.hidden_dim * cfg.dim;
    w.w3 = ptr;                    ptr += cfg.n_layers * cfg.dim * cfg.hidden_dim;
    w.rms_final_weight = ptr;      ptr += cfg.dim;
    w.freq_cis_real = ptr;         ptr += cfg.seq_len * head_size / 2;
    w.freq_cis_imag = ptr;         ptr += cfg.seq_len * head_size / 2;
    w.wcls = shared_weights ? w.token_embedding_table : ptr;

    /* 5. Allocate run state buffers on heap */
    struct run_state s;
    s.x      = (float *)bump_alloc(&h, cfg.dim * 4);
    s.xb     = (float *)bump_alloc(&h, cfg.dim * 4);
    s.xb2    = (float *)bump_alloc(&h, cfg.dim * 4);
    s.hb     = (float *)bump_alloc(&h, cfg.hidden_dim * 4);
    s.hb2    = (float *)bump_alloc(&h, cfg.hidden_dim * 4);
    s.q      = (float *)bump_alloc(&h, cfg.dim * 4);
    s.k      = (float *)bump_alloc(&h, kv_dim * 4);
    s.v      = (float *)bump_alloc(&h, kv_dim * 4);
    s.att    = (float *)bump_alloc(&h, cfg.n_heads * cfg.seq_len * 4);
    s.logits = (float *)bump_alloc(&h, cfg.vocab_size * 4);
    s.key_cache   = (float *)bump_alloc(&h, cfg.n_layers * cfg.seq_len * kv_dim * 4);
    s.value_cache = (float *)bump_alloc(&h, cfg.n_layers * cfg.seq_len * kv_dim * 4);

    if (!s.x || !s.value_cache) {
        coconut_puts("llama-inference: heap OOM for run state\n");
        return 1;
    }

    /* 6. Run inference for 16 tokens */
    int token = 0; /* start token */
    int n_tokens = 16;

    for (int pos = 0; pos < n_tokens; pos++) {
        forward(&cfg, &w, &s, token, pos);
        int next = argmax(s.logits, cfg.vocab_size);

        coconut_puts("llama-inference: token ");
        print_u64(pos);
        coconut_puts(" -> '");
        char ch = (next >= 0 && next < 32) ? VOCAB[next] : '?';
        coconut_serial_write(&ch, 1);
        coconut_puts("'\n");

        token = next;
    }

    coconut_puts("llama-inference: inference complete (16 tokens)\n");
    return 0;
}
