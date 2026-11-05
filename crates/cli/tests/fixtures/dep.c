// A dependency of basic.c, proving DT_NEEDED resolution and initialization order.
int dep_initialized;

__attribute__((constructor)) static void on_load(void) { dep_initialized = 1; }

int dep_value(void) { return 7; }
