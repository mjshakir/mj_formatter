#pragma once
//--------------------------------------------------------------
// Standard Cpp Libraries
//--------------------------------------------------------------
#include <array>
#include <atomic>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <memory>
#include <optional>

//--------------------------------------------------------------
// User Defined Headers
//--------------------------------------------------------------
#include "HazardSystemAPI.hpp"
namespace HazardSystem {
    //--------------------------------------------------------------
    // Lock-free hierarchical summary over one or more bitsets ("planes").
    // - Leaf bit i == 1 means "present" in that plane.
    // - Internal levels summarize non-empty 64-bit words of the level below.
    // - Operations are atomic; lock-free if std::atomic<uint64_t> is lock-free.
    //--------------------------------------------------------------
    class HAZARDSYSTEM_API BitmapTree {
        //--------------------------------------------------------------
        private:
            static constexpr size_t S_C_WORD_BITS   = static_cast<size_t>(std::numeric_limits<uint64_t>::digits);
            static constexpr size_t S_C_LEVEL_SHIFT = 6UL; // log2(64)
            //--------------------------
            static_assert((1ULL << S_C_LEVEL_SHIFT) == S_C_WORD_BITS, "BitmapTree assumes 64-bit words");
            //--------------------------
            static constexpr size_t S_C_MAX_PLANES = 2UL;
            static constexpr size_t S_C_MAX_LEVELS = (S_C_WORD_BITS + (S_C_LEVEL_SHIFT - 1)) / S_C_LEVEL_SHIFT;
            //--------------------------
            enum class Mode : uint8_t {Empty = 1 << 0, SingleWord = 1 << 1, Tree = 1 << 2};
        //--------------------------------------------------------------
        public:
            //--------------------------------------------------------------
            BitmapTree(void) noexcept;
            //--------------------------
            BitmapTree(const BitmapTree&)            = delete;
            BitmapTree& operator=(const BitmapTree&) = delete;
            //--------------------------
            BitmapTree(BitmapTree&& other) noexcept;
            BitmapTree& operator=(BitmapTree&& other) noexcept;
            //--------------------------
            ~BitmapTree(void)                                   = default;
            //--------------------------------------------------------------
            bool initialization(const size_t& leaf_bits);
            //--------------------------
            bool initialization(const size_t& leaf_bits, const size_t& planes);
            //--------------------------
            bool reset_set(const size_t& plane = 0) noexcept;
            //--------------------------
            bool reset_clear(const size_t& plane = 0) noexcept;
            //--------------------------
            bool set(const size_t& bit_index, const size_t& plane = 0) noexcept;
            //--------------------------
            bool clear(const size_t& bit_index, const size_t& plane = 0) noexcept;
            //--------------------------
            std::optional<size_t> find(const size_t& hint = 0) const noexcept;
            //--------------------------
            std::optional<size_t> find(const size_t& hint, const size_t& plane) const noexcept;
            //--------------------------
            // Like find, but does not wrap; searches [start, leaf_bits()) only.
            std::optional<size_t> find_next(const size_t& start, const size_t& plane = 0) const noexcept;
            //--------------------------
            size_t leaf_bits(void) const noexcept;
            //--------------------------
            size_t planes(void) const noexcept;
            //--------------------------------------------------------------
        protected:
            //--------------------------------------------------------------
            bool initialization_data(const size_t& leaf_bits);
            //--------------------------
            bool initialization_data(const size_t& leaf_bits, const size_t& planes);
            //--------------------------
            bool reset_all_set(const size_t& plane) noexcept;
            //--------------------------
            bool reset_all_clear(const size_t& plane) noexcept;
            //--------------------------
            bool set_data(const size_t& bit_index, const size_t& plane) noexcept;
            //--------------------------
            bool clear_data(const size_t& bit_index, const size_t& plane) noexcept;
            //--------------------------
            std::optional<size_t> find_data(const size_t& hint, const size_t& plane) const noexcept;
            //--------------------------
            std::optional<size_t> find_next_data(const size_t& start, const size_t& plane) const noexcept;
            //--------------------------
            size_t leaf_bits_data(void) const noexcept;
            //--------------------------
            size_t planes_data(void) const noexcept;
            //--------------------------
            void reset_data(void) noexcept;
            //--------------------------
            void build_layout(void);
            //--------------------------
            std::atomic<uint64_t>& word_data(const size_t& plane, const size_t& level, const size_t& word_index) noexcept;
            const std::atomic<uint64_t>& word_data(const size_t& plane, const size_t& level, const size_t& word_index) const noexcept;
            //--------------------------
            bool set_bit(const size_t& plane, const size_t& level, const size_t& bit_index) noexcept;
            //--------------------------
            bool clear_bit(const size_t& plane, const size_t& level, const size_t& bit_index) noexcept;
            //--------------------------
            std::optional<size_t> find_next_set_bit(const size_t& plane, const size_t& level, const size_t& start_bit) const noexcept;
            //--------------------------
            std::optional<size_t> find_from_leaf(const size_t& plane, const size_t& start_leaf_bit) const noexcept;
            //--------------------------------------------------------------
        private:
            //--------------------------------------------------------------
            Mode _m_mode;
            size_t _m_leaf_bits, _m_planes, _m_levels, _m_words_per_plane;
            //--------------------------
            std::array<std::atomic<uint64_t>, S_C_MAX_PLANES> m_single;
            std::array<size_t, S_C_MAX_LEVELS> _m_level_words, _m_level_offsets;
            std::unique_ptr<std::atomic<uint64_t>[]> m_tree_words;
        //--------------------------------------------------------------
    }; // end BitmapTree(...)
    //--------------------------------------------------------------
} // end namespace HazardSystem
//--------------------------------------------------------------
