// Proves the NDK's C++ runtime works: static constructors, allocation and exception unwinding.
#include <stdexcept>
#include <string>
#include <thread>
#include <vector>

namespace {
const std::string kGlobal = std::string("static ") + "constructor";
}

extern "C" int test_static_constructor() { return kGlobal == "static constructor" ? 0 : 1; }

extern "C" int test_exception() {
    try {
        throw std::runtime_error("unwound");
    } catch (const std::exception &error) {
        return std::string(error.what()) == "unwound" ? 0 : 1;
    }
    return 2;
}

extern "C" int test_containers() {
    std::vector<std::string> values(1000, "x");
    return values.back() == "x" ? 0 : 1;
}

extern "C" int test_std_thread() {
    int value = 0;
    std::thread worker([&value] { value = 3; });
    worker.join();
    return value == 3 && std::thread::hardware_concurrency() >= 1 ? 0 : 1;
}
