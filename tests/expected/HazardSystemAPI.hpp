#pragma once

#if defined(HAZARDSYSTEM_STATIC)
    #define HAZARDSYSTEM_API
    #define HAZARDSYSTEM_LOCAL
#elif defined(_WIN32) || defined(__CYGWIN__)
    #if defined(HAZARDSYSTEM_BUILDING_LIBRARY)
        #define HAZARDSYSTEM_API __declspec(dllexport)
    #else
        #define HAZARDSYSTEM_API __declspec(dllimport)
    #endif
    #define HAZARDSYSTEM_LOCAL
#else
    #if defined(__GNUC__) || defined(__clang__)
        #define HAZARDSYSTEM_API __attribute__((visibility("default")))
        #define HAZARDSYSTEM_LOCAL __attribute__((visibility("hidden")))
    #else
        #define HAZARDSYSTEM_API
        #define HAZARDSYSTEM_LOCAL
    #endif
#endif
