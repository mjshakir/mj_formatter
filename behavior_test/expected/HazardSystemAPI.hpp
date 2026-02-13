#pragma once

#if defined(_WIN32) && defined(BUILD_HAZARDSYSTEM_DLL)
#define HAZARDSYSTEM_API __declspec(dllexport)
#elif defined(_WIN32)
#define HAZARDSYSTEM_API __declspec(dllimport)
#else
#define HAZARDSYSTEM_API __attribute__((visibility("default")))
#endif
