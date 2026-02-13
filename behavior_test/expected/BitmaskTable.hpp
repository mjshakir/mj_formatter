#pragma once
//--------------------------------------------------------------
// Standard C++ library
//--------------------------------------------------------------
#include <cstddef>
#include <cstdint>
#include <array>
#include <vector>
#include <atomic>
#include <memory>
#include <optional>
#include <bit>
#include <type_traits>
#include <limits>
#include <utility>
//--------------------------------------------------------------
// User Defined Headers
//--------------------------------------------------------------
#include "HazardPointer.hpp"
#include "BitmapTree.hpp"
//--------------------------------------------------------------
namespace HazardSystem {
    //--------------------------------------------------------------
    template<typename T, uint16_t N = 0>
    class BitmaskTable {
        //--------------------------------------------------------------
        private:
            //--------------------------------------------------------------
#if defined(BUILDHAZARDSYSTEMDISABLEBITMASKROTATION)
            static constexpr bool S_C_ENABLE_ROTATION     = false;
#else
            static constexpr bool S_C_ENABLE_ROTATION     = true;
#endif
            //--------------------------
            static constexpr uint16_t S_C_ARRAY_LIMIT     = 1024U;
            //--------------------------
            static constexpr uint16_t S_C_BITS_PER_MASK   = std::numeric_limits<uint64_t>::digits;

            static constexpr uint8_t S_C_ROTATE_THRESHOLD = static_cast<uint8_t>(S_C_BITS_PER_MASK / 2);
            static constexpr uint16_t S_C_MASK_COUNT      = (N == 0 ? 0 : static_cast<uint16_t>((N + S_C_BITS_PER_MASK - 1) / S_C_BITS_PER_MASK));
            //--------------------------
            static constexpr bool S_C_TREE_POSSIBLE = (N == 0) or (N > S_C_ARRAY_LIMIT);
            static constexpr bool S_C_TREE_ALWAYS   = (N > S_C_ARRAY_LIMIT);
            //--------------------------
            struct NoTree {
            }; // end struct NoTree
            //--------------------------
            using TreeStorage                            = std::conditional_t<S_C_TREE_POSSIBLE,
                                                            std::conditional_t<S_C_TREE_ALWAYS, BitmapTree, std::unique_ptr<BitmapTree>>,
                                                            NoTree>;
            //--------------------------
            using SlotType                              = std::conditional_t<(N == 0) or (N > S_C_ARRAY_LIMIT),
                                                            std::vector<HazardPointer<T>>, std::array<HazardPointer<T>, N>>;
            //--------------------------
            enum class PartPlane : uint8_t {Available = 0, NonEmpty = 1, Count = 2};
            //--------------------------
            template<uint16_t M, bool USE_SIZE = (M == 0) or (M > S_C_ARRAY_LIMIT)>
            struct IndexTypeSelector;
            //--------------------------
            template<uint16_t M>
            struct IndexTypeSelector<M, true> {
                using type = size_t;
            };// end struct IndexTypeSelector
            //--------------------------
            template<uint16_t M>
            struct IndexTypeSelector<M, false> {
                using type = std::conditional_t<(M <= std::numeric_limits<uint8_t>::max()), uint8_t, uint16_t>;
            };// end struct IndexTypeSelector
            //--------------------------------------------------------------
        public:
            //--------------------------------------------------------------
            using IndexType                 = typename IndexTypeSelector<N>::type;
            //--------------------------
            using iterator               = typename SlotType::iterator;
            using const_iterator         = typename SlotType::const_iterator;
            using reverse_iterator       = typename SlotType::reverse_iterator;
            using const_reverse_iterator = typename SlotType::const_reverse_iterator;
            //--------------------------------------------------------------
        public:
            //--------------------------------------------------------------
            template <uint16_t M = N, std::enable_if_t<(M == 0), int> = 0>
            BitmaskTable(void) :    m_a_capacity(0UL),
                                    m_a_mask_count(0UL),
                                    m_a_size(0UL),
                                    m_slots(),
                                    m_bitmask(),
                                    m_available(),
                                    m_use_tree(false),
                                    m_c_initialize(false) {
                //--------------------------
            } // end BitmaskTable(...)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t<(M > 0) and (M <= 64), int> = 0>
            BitmaskTable(void) :    m_a_capacity(0UL),
                                    m_a_mask_count(0UL),
                                    m_a_size(0UL),
                                    m_slots(),
                                    m_bitmask(initial_bitmask()),
                                    m_available(),
                                    m_use_tree(false),
                                    m_c_initialize(false) {
                //--------------------------
            } // end BitmaskTable(...)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t< (M > 64) and (M <= S_C_ARRAY_LIMIT), int> = 0>
            BitmaskTable(void) :    m_a_capacity(0UL),
                                    m_a_mask_count(0UL),
                                    m_a_size(0UL),
                                    m_slots(),
                                    m_bitmask(),
                                    m_available(),
                                    m_use_tree(false),
                                    m_c_initialize(initialization(0ULL) and maybe_initialize_tree(static_cast<size_t>(get_mask_count()))) {
                //--------------------------
            } // end BitmaskTable(...)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t< (M == 0), int> = 0>
            BitmaskTable(const size_t& capacity) :  m_a_capacity(bitmask_capacity(capacity)),
                                                    m_a_mask_count(bitmask_calculator(bitmask_capacity(capacity))),
                                                    m_a_size(0UL),
                                                    m_slots(bitmask_capacity(capacity)),
                                                    m_bitmask(bitmask_calculator(bitmask_capacity(capacity))),
                                                    m_available(),
                                                    m_use_tree(use_tree(bitmask_capacity(capacity))),
                                                    m_c_initialize(initialization(0ULL) and maybe_initialize_tree(static_cast<size_t>(get_mask_count()))) {
                //--------------------------
            } // end BitmaskTable(...)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t< (M > S_C_ARRAY_LIMIT), int> = 0>
            BitmaskTable(void) :    m_a_capacity(bitmask_capacity(N)),
                                    m_a_mask_count(bitmask_calculator(bitmask_capacity(N))),
                                    m_a_size(0UL),
                                    m_slots(bitmask_capacity(N)),
                                    m_bitmask(bitmask_calculator(bitmask_capacity(N))),
                                    m_available(),
                                    m_use_tree(use_tree(bitmask_capacity(N))),
                                    m_c_initialize(initialization(0ULL) and maybe_initialize_tree(static_cast<size_t>(get_mask_count()))) {
                //--------------------------
            } // end BitmaskTable(...)
            //--------------------------
            ~BitmaskTable(void)                           = default;
            //--------------------------
            BitmaskTable(const BitmaskTable&)             = delete;
            BitmaskTable& operator=(const BitmaskTable&)  = delete;
            BitmaskTable(BitmaskTable&&)                  = default;
            BitmaskTable& operator=(BitmaskTable&&)       = default;
            //--------------------------------------------------------------
        public:
            //--------------------------------------------------------------
            std::optional<IndexType> acquire(void) {
                return acquire_data();
            } // end std::optional<IndexType> acquire(void)
            //--------------------------
            std::optional<iterator> acquire_iterator(void) {
                return acquire_data_iterator();
            } // end std::optional<iterator> acquire_iterator(void)
            //--------------------------
            std::optional<const_iterator> acquire_iterator(void) const {
                return acquire_data_iterator();
            } // end std::optional<const_iterator> acquire_iterator(void) const
            //--------------------------
            bool acquire(iterator it) {
                return reacquire_iterator(it);
            } // end bool acquire(iterator it)
            //--------------------------
            bool release(const IndexType& index) {
                return release_data(index);
            } // end bool release(const IndexType& index)
            //--------------------------
            bool release(const std::optional<IndexType>& index) {
                //--------------------------
                if(!index.has_value()) {
                    return false;
                }// end if(!index.has_value())
                //--------------------------
                return release_data(index.value());
                //--------------------------
            } // end bool release(const std::optional<IndexType>& index)
            //--------------------------
            bool set(const IndexType& index, T* ptr) {
                return set_data(index, ptr);
            } // end bool set(const IndexType& index, T* ptr)
            //--------------------------
            bool set(const std::optional<IndexType>& index, T* ptr) {
                //--------------------------
                if(!index.has_value()) {
                    return false;
                }// end if(!index.has_value())
                //--------------------------
                return set_data(index.value(), ptr);
                //--------------------------
            } // end bool set(const std::optional<IndexType>& index, T* ptr)
            //--------------------------
            std::optional<IndexType> set(T* ptr) {
                return set_data(ptr);
            } // end std::optional<IndexType> set(T* ptr)
            //--------------------------
            bool set(const_iterator it, T* ptr) {
                return set_data(it, ptr);
            } // end bool set(const_iterator it, T* ptr)
            //--------------------------
            T* at(const IndexType& index) const {
                return at_data(index);
            } // end T* at(const IndexType& index) const
            //--------------------------
            T* at(const std::optional<IndexType>& index) const {
                //--------------------------
                if(!index.has_value()) {
                    return nullptr;
                }// end if(!index.has_value())
                //--------------------------
                return at_data(index.value());
                //--------------------------
            } // end T* at(const std::optional<IndexType>& index) const
            //--------------------------
            bool active(const IndexType& index) const {
                return active_data(index);
            } // end bool active(const IndexType& index) const
            //--------------------------
            bool active(const std::optional<IndexType>& index) const {
                //--------------------------
                if(!index.has_value()) {
                    return false;
                }// end if(!index.has_value())
                //--------------------------
                return active_data(index.value());
                //--------------------------
            } // end bool active(const std::optional<IndexType>& index) const
            //--------------------------
            template <typename Fn>
            void for_each(Fn&& fn) const {
                for_each_active(std::forward<Fn>(fn));
            } // end void for_each(Fn&& fn) const
            //--------------------------
            template <typename Fn>
            void for_each_fast(Fn&& fn) const {
                for_each_active_fast(std::forward<Fn>(fn));
            } // end void for_each_fast(Fn&& fn) const
            //--------------------------
            template <typename Fn>
            bool find(Fn&& fn) const {
                return find_data(std::forward<Fn>(fn));
            } // end bool find(Fn&& fn) const
            //--------------------------
            void clear(void) {
                clear_data();
            } // end void clear(void)
            //--------------------------
            IndexType size(void) const {
                return size_data();
            } // end IndexType size(void) const
            //--------------------------
            constexpr IndexType capacity(void) const {
                return get_capacity();
            } // end constexpr IndexType capacity(void) const
            //--------------------------
            iterator begin(void) noexcept {
                return m_slots.begin();
            } // end iterator begin(void) noexcept
            //--------------------------
            iterator end(void) noexcept {
                return m_slots.end();
            } // end iterator end(void) noexcept
            //--------------------------
            const_iterator begin(void) const noexcept {
                return m_slots.begin();
            } // end const_iterator begin(void) const noexcept
            //--------------------------
            const_iterator end(void) const noexcept {
                return m_slots.end();
            } // end const_iterator end(void) const noexcept
            //--------------------------
            const_iterator cbegin(void) const noexcept {
                return m_slots.cbegin();
            } // end const_iterator cbegin(void) const noexcept
            //--------------------------
            const_iterator cend(void) const noexcept {
                return m_slots.cend();
            } // end const_iterator cend(void) const noexcept
            //--------------------------
            reverse_iterator rbegin(void) noexcept {
                return m_slots.rbegin();
            } // end reverse_iterator rbegin(void) noexcept
            //--------------------------
            reverse_iterator rend(void) noexcept {
                return m_slots.rend();
            } // end reverse_iterator rend(void) noexcept
            //--------------------------
            const_reverse_iterator rbegin(void) const noexcept {
                return m_slots.rbegin();
            } // end const_reverse_iterator rbegin(void) const noexcept
            //--------------------------
            const_reverse_iterator rend(void) const noexcept {
                return m_slots.rend();
            } // end const_reverse_iterator rend(void) const noexcept
            //--------------------------
            const_reverse_iterator crbegin(void) const noexcept {
                return m_slots.crbegin();
            } // end const_reverse_iterator crbegin(void) const noexcept
            //--------------------------
            const_reverse_iterator crend(void) const noexcept {
                return m_slots.crend();
            } // end const_reverse_iterator crend(void) const noexcept
            //--------------------------------------------------------------
        protected:
            //--------------------------------------------------------------
            // Core operations
            //--------------------------------------------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M > 0) and (M <= 64), std::optional<IndexType>> acquire_data(void) {
                //--------------------------
                uint64_t mask = m_bitmask.load(std::memory_order_relaxed);
                //--------------------------
                while (mask != ~0ULL) {
                    //--------------------------
                    IndexType index = static_cast<IndexType>(std::countr_zero(~mask));
                    //--------------------------
                    if (index >= static_cast<IndexType>(N)) {
                        break;
                    }// end if (index >= static_cast<IndexType>(N))
                    //--------------------------
                    uint64_t flag    = 1ULL << index;
                    uint64_t desired = mask | flag;
                    //--------------------------
                    if (m_bitmask.compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)) {
                        m_a_size.fetch_add(1, std::memory_order_relaxed);
                        return index;
                    }// end if (m_bitmask.compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)))
                }// end while (mask != ~0ULL)
                //--------------------------
                return std::nullopt;
                //--------------------------
            } // end std::enable_if_t<(M > 0) and (M <= 64), std::optional<IndexType>> acquire_data(void)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), std::optional<IndexType>> acquire_data(void) {
                //--------------------------
                const IndexType capacity        = get_capacity();
                const IndexType _c_mask_count   = get_mask_count();
                const size_t capacity_size      = static_cast<size_t>(capacity);
                const size_t _c_mask_count_size = static_cast<size_t>(_c_mask_count);
                //--------------------------
                if (!capacity or !_c_mask_count) {
                    return std::nullopt;
                }// end if (!capacity or !mask_count)
                //--------------------------
                const size_t _c_available_plane = plane_index(PartPlane::Available);
                const bool _use_tree            = tree_enabled();
                BitmapTree* tree                = _use_tree ? tree_ptr() : nullptr;
                thread_local size_t part_hint   = 0;
                thread_local uint8_t _bit_hint  = 0;
                size_t _start_part              = part_hint % _c_mask_count_size;
                //--------------------------
                while (m_a_size.load(std::memory_order_relaxed) < capacity_size) {
                    std::optional<size_t> part_opt;
                    if (_use_tree) {
                        part_opt = tree->find(_start_part, _c_available_plane);
                        if (!part_opt) {
                            // Tree is a hint; fall back to a bounded scan to avoid spurious failures under contention.
                            //--------------------------
                            if (m_a_size.load(std::memory_order_relaxed) >= capacity_size) {
                                return std::nullopt;
                            }// end if (m_size.load(std::memory_order_relaxed) >= capacity)
                            //--------------------------
                            part_opt = scan_available(_start_part, _c_mask_count_size, _c_available_plane);
                            if (!part_opt) {
                                return std::nullopt;
                            }// end if (!part_opt)
                        }// end if (!part_opt)
                    } else {
                        part_opt = scan_available(_start_part, _c_mask_count_size, _c_available_plane);
                        if (!part_opt) {
                            return std::nullopt;
                        }// end if (!part_opt)
                    }// end if (_use_tree)
                    //--------------------------
                    const IndexType part = static_cast<IndexType>(part_opt.value());
                    part_hint            = static_cast<size_t>(part);
                    _start_part          = (static_cast<size_t>(part) + 1) % _c_mask_count_size;
                    uint64_t mask        = m_bitmask[part].load(std::memory_order_relaxed);
                    //--------------------------
                    while (mask != ~0ULL) {
                        //--------------------------
                        const uint8_t bit          = select_free_bit(mask, _bit_hint);
                        const IndexType slot_index = static_cast<IndexType>((part * S_C_BITS_PER_MASK) + bit);
                        //--------------------------
                        if (slot_index >= capacity) {
                            break;
                        }// end if (slot_index >= capacity)
                        //--------------------------
                        const uint64_t flag    = 1ULL << bit;
                        const uint64_t desired = mask | flag;
                        //--------------------------
                        if (m_bitmask[part].compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)) {
                            m_a_size.fetch_add(1, std::memory_order_relaxed);
                            static_cast<void>(mark_non_empty(part));
                            if constexpr (S_C_ENABLE_ROTATION) {
                                _bit_hint = static_cast<uint8_t>((bit + 1) % S_C_BITS_PER_MASK);
                            }// end if constexpr (C_ENABLE_ROTATION)
                            static_cast<void>(update_on_full(part, desired, _c_available_plane));
                            return slot_index;
                        }// end if (m_bitmask[part].compare_exchange_weak(...))
                    }// end while (mask != ~0ULL)
                    //--------------------------
                    // Part is (now) full; clear and retry.
                    static_cast<void>(refresh_hint(part, _c_available_plane));
                }// end while (m_size.load(std::memory_order_relaxed) < capacity)
                return std::nullopt;
                //--------------------------
            } // end acquire_data(...)
            //--------------------------
            std::optional<iterator> acquire_data_iterator(void) {
                //--------------------------
                auto _index = acquire_data();
                if (!_index) {
                    return std::nullopt;
                }// end if (!_index)
                //--------------------------
                return m_slots.begin() + static_cast<typename SlotType::difference_type>(_index.value());
                //--------------------------
            } // end std::optional<iterator> acquire_data_iterator(void)
            //--------------------------
            std::optional<const_iterator> acquire_data_iterator(void) const {
                //--------------------------
                auto _index = acquire_data();
                if (!_index) {
                    return std::nullopt;
                }// end if (!_index)
                //--------------------------
                return m_slots.begin() + static_cast<typename SlotType::difference_type>(_index.value());
                //--------------------------
            } // end std::optional<const_iterator> acquire_data_iterator(void) const
            //--------------------------
            bool reacquire_iterator(const_iterator it) {
                //--------------------------
                const auto first = m_slots.begin();
                const auto last  = m_slots.end();
                //--------------------------
                if (it < first or it >= last) {
                    return false;
                }// end if (it < first or it >= last)
                //--------------------------
                const IndexType index = static_cast<IndexType>(it - first);
                if (index >= get_capacity()) {
                    return false;
                }// end if (index >= get_capacity())
                //--------------------------
                if (m_slots[index].load(std::memory_order_acquire)) {
                    return false;
                }// end if (m_slots[index].load(std::memory_order_acquire))
                //--------------------------
                return reacquire_index(index);
                //--------------------------
            } // end bool reacquire_iterator(const_iterator it)
            //--------------------------
            bool release_data(const IndexType& index) {
                //--------------------------
                if (index >= get_capacity()) {
                    return false;
                }// end if (index >= get_capacity())
                //--------------------------
                m_slots[index].store(nullptr, std::memory_order_release);
                if constexpr ((N > 0) and (N <= 64)) {
                    //--------------------------
                    const uint64_t bit    = 1ULL << index;
                    const uint64_t _c_old = m_bitmask.fetch_and(~bit, std::memory_order_acq_rel);
                    if ((_c_old & bit) == 0) {
                        return false;
                    }// end if ((old & bit) == 0)
                } else {
                    //--------------------------
                    const IndexType part = part_index(index);
                    const uint16_t bit   = bit_index(index);
                    //--------------------------
                    const uint64_t flag   = 1ULL << bit;
                    const uint64_t _c_old = m_bitmask[part].fetch_and(~flag, std::memory_order_acq_rel);
                    if ((_c_old & flag) == 0) {
                        return false;
                    }// end if ((old & flag) == 0)
                    static_cast<void>(available_not_full(part, _c_old, plane_index(PartPlane::Available)));
                    //--------------------------
                }// end if constexpr ((N > 0) and (N <= 64))
                //--------------------------
                m_a_size.fetch_sub(1, std::memory_order_relaxed);
                return true;
                //--------------------------
            } // end bool release_data(const IndexType& index)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M > 0) and (M <= 64) , bool> set_data(const IndexType& index, T* ptr) {
                //--------------------------
                if (index >= get_capacity()) {
                    return false;
                }// end if (index >= get_capacity())
                //--------------------------
                m_slots[index].store(ptr, std::memory_order_release);
                //--------------------------
                const uint64_t bit = 1ULL << index;
                //--------------------------
                if (ptr) {
                    const uint64_t _c_old = m_bitmask.fetch_or(bit, std::memory_order_acq_rel);
                    if ((_c_old & bit) == 0) {
                        m_a_size.fetch_add(1, std::memory_order_relaxed);
                    }// end if ((old & bit) == 0)
                } else {
                    const uint64_t _c_old = m_bitmask.fetch_and(~bit, std::memory_order_acq_rel);
                    if (_c_old & bit) {
                        m_a_size.fetch_sub(1, std::memory_order_relaxed);
                    }// end if (old & bit)
                }// end  if (ptr)
                //--------------------------
                return true;
                //--------------------------
            } // end std::enable_if_t<(M > 0) and (M <= 64) , bool> set_data(const IndexType& index, T* ptr)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool> set_data(const IndexType& index, T* ptr) {
                //--------------------------
                if (index >= get_capacity()) {
                    return false;
                }// end if (index >= get_capacity())
                //--------------------------
                m_slots[index].store(ptr, std::memory_order_release);
                //--------------------------
                const IndexType part   = part_index(index);
                const uint16_t bit     = bit_index(index);
                const uint64_t bitmask = 1ULL << bit;
                //--------------------------
                if (ptr) {
                    //--------------------------
                    const uint64_t _c_old = m_bitmask[part].fetch_or(bitmask, std::memory_order_acq_rel);
                    const bool marked     = mark_non_empty(part);
                    //--------------------------
                    if ((_c_old & bitmask) == 0) {
                        m_a_size.fetch_add(1, std::memory_order_relaxed);
                    }// end if ((old & bitmask) == 0)
                    const uint64_t now = _c_old | bitmask;
                    if (marked) {
                        static_cast<void>(update_on_full(part, now, plane_index(PartPlane::Available)));
                    }
                } else {
                    const uint64_t _c_old = m_bitmask[part].fetch_and(~bitmask, std::memory_order_acq_rel);
                    if (_c_old & bitmask) {
                        m_a_size.fetch_sub(1, std::memory_order_relaxed);
                    }// end if (old & bitmask)
                    static_cast<void>(available_not_full(part, _c_old, plane_index(PartPlane::Available)));
                }// end if (ptr)
                //--------------------------
                return true;
                //--------------------------
            } // end std::enable_if_t<(M == 0) or (M > 64), bool> set_data(const IndexType& index, T* ptr)
            //--------------------------
            std::optional<IndexType> set_data(T* ptr) {
                //--------------------------
                if (!ptr) {
                    return std::nullopt;
                }// end if (!ptr)
                //--------------------------
                std::optional<IndexType> _index = acquire_data();
                //--------------------------
                if (!_index) {
                    return std::nullopt;
                }// end if (!_index)
                //--------------------------
                set_data(_index.value(), ptr);
                //--------------------------
                return _index;
                //--------------------------
            } // end std::optional<IndexType> set_data(T* ptr)
            //--------------------------
            bool set_data(const_iterator it, T* ptr) {
                //--------------------------
                auto first = m_slots.begin();
                //--------------------------
                if (it < first or it >= m_slots.end()) {
                    return false;
                }// end if (it < first or it >= m_slots.end())
                //--------------------------
                return set_data(static_cast<IndexType>(it - first), ptr);
                //--------------------------
            } // end bool set_data(const_iterator it, T* ptr)
            //--------------------------
            T* at_data(const IndexType& index) const {
                //--------------------------
                if (index >= get_capacity()) {
                    return nullptr;
                }// end if (index >= get_capacity())
                //--------------------------
                return m_slots[index].load(std::memory_order_acquire);
                //--------------------------
            } // end T* at_data(const IndexType& index) const
            //--------------------------
            bool active_data(const IndexType& index) const {
                //--------------------------
                if (index >= get_capacity()) {
                    return false;
                }// end if (index >= get_capacity())
                //--------------------------
                uint64_t mask = 0;
                //--------------------------
                if constexpr ((N > 0) and (N <= 64)) {
                    mask = m_bitmask.load(std::memory_order_acquire);
                    return (mask & (1ULL << index)) != 0;
                } else {
                    //--------------------------
                    const IndexType part = part_index(index);
                    const uint16_t bit   = bit_index(index);
                    mask                 = m_bitmask[part].load(std::memory_order_acquire);
                    return (mask & (1ULL << bit)) != 0;
                    //--------------------------
                }// end if constexpr (N <= 64)
            } // end bool active_data(const IndexType& index) const
            //--------------------------
            IndexType active_count_data(void) const {
                //--------------------------
                if constexpr ((N > 0) and (N <= 64)) {
                    uint64_t mask = m_bitmask.load(std::memory_order_acquire);
                    return static_cast<IndexType>(std::popcount(mask));
                }// end if constexpr (N <= 64)
                //--------------------------
                IndexType _count = 0;
                //--------------------------
                for (const auto& mask : m_bitmask) {
                    _count += static_cast<IndexType>(std::popcount(mask.load(std::memory_order_acquire)));
                }// end for (const auto& mask : m_bitmask)
                //--------------------------
                return _count;
                //--------------------------
            } // end IndexType active_count_data(void) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M > 0) and (M <= 64), void> for_each_active(Fn&& fn) const {
                //--------------------------
                const uint64_t mask = m_bitmask.load(std::memory_order_acquire);
                //--------------------------
                for (IndexType index = 0; index < N; ++index) {
                    //--------------------------
                    if (mask & (1ULL << index)) {
                        auto ptr = m_slots[index].load(std::memory_order_acquire);
                        if (ptr) {
                            fn(index, ptr);
                        }// end if (ptr)
                    }// end if (mask & (1ULL << index))
                    //--------------------------
                }// end for (IndexType index = 0; index < N; ++index)
                //--------------------------
            } // end std::enable_if_t<(M > 0) and (M <= 64), void> for_each_active(Fn&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M == 0) or (M > 64), void> for_each_active(Fn&& fn) const {
                //--------------------------
                for (IndexType part = 0; part < get_mask_count(); ++part) {
                    //--------------------------
                    const uint64_t mask  = m_bitmask[part].load(std::memory_order_acquire);
                    const IndexType base = static_cast<IndexType>(part * S_C_BITS_PER_MASK);
                    //--------------------------
                    for (uint8_t bit = 0; bit < S_C_BITS_PER_MASK; ++bit) {
                        //--------------------------
                        IndexType index = base + bit;
                        if (index >= get_capacity()) {
                            break;
                        }// end if (index >= get_capacity())
                        //--------------------------
                        if (mask & (1ULL << bit)) {
                            auto ptr = m_slots[index].load(std::memory_order_acquire);
                            if (ptr) {
                                fn(index, ptr);
                            }// end if (ptr)
                        }// end if (mask & (1ULL << bit))
                        //--------------------------
                    }// end for (uint8_t bit = 0; bit < C_BITS_PER_MASK; ++bit)
                }// end for (uint16_t part = 0; part < C_MASK_COUNT; ++part)
            } // end std::enable_if_t<(M == 0) or (M > 64), void> for_each_active(Fn&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M > 0) and (M <= 64), void> for_each_active_fast(Fn&& fn) const {
                //--------------------------
                uint64_t mask = m_bitmask.load(std::memory_order_acquire);
                //--------------------------
                while (mask) {
                    //--------------------------
                    const uint8_t _index = static_cast<uint8_t>(std::countr_zero(mask));
                    //--------------------------
                    if (_index < get_capacity()) {
                        auto ptr = m_slots[_index].load(std::memory_order_acquire);
                        if (ptr) {
                            fn(_index, ptr);
                        }// end if (ptr)
                    }// end if (_index < get_capacity())
                    //--------------------------
                    mask &= mask - 1; // Clear the lowest set bit
                    //--------------------------
                }// end while (mask)
                //--------------------------
            } // end std::enable_if_t<(M > 0) and (M <= 64), void> for_each_active_fast(Fn&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M == 0) or (M > 64), void> for_each_active_fast(Fn&& fn) const {
                //--------------------------
                const IndexType _c_mask_count = get_mask_count();
                const IndexType capacity      = get_capacity();
                //--------------------------
                if (!_c_mask_count) {
                    return;
                }// end if (!mask_count)
                //--------------------------
                if (!tree_enabled()) {
                    for (IndexType part = 0; part < _c_mask_count; ++part) {
                        //--------------------------
                        uint64_t mask = m_bitmask[part].load(std::memory_order_acquire);
                        //--------------------------
                        if (!mask) {
                            continue;
                        }// end if (!mask)
                        //--------------------------
                        const IndexType base = static_cast<IndexType>(part * S_C_BITS_PER_MASK);
                        while (mask) {
                            const IndexType index = base + static_cast<uint8_t>(std::countr_zero(mask));
                            if (index >= capacity) {
                                break;
                            }// end if (index >= capacity)
                            //--------------------------
                            auto ptr = m_slots[index].load(std::memory_order_acquire);
                            if (ptr) {
                                fn(index, ptr);
                            }// end if (ptr)
                            //--------------------------
                            mask &= mask - 1;
                        }// end  while (mask)
                    }// end for (IndexType part = 0; part < mask_count; ++part)
                    return;
                }// end if (!tree_enabled)
                //--------------------------
                size_t hint      = 0;
                BitmapTree* tree = tree_ptr();
                for (auto part_opt = tree->find_next(hint, plane_index(PartPlane::NonEmpty));
                        part_opt;
                        part_opt = tree->find_next(hint, plane_index(PartPlane::NonEmpty))) {
                    //--------------------------
                    const IndexType part = static_cast<IndexType>(part_opt.value());
                    uint64_t mask        = m_bitmask[part].load(std::memory_order_acquire);
                    //--------------------------
                    if (!mask) {
                        static_cast<void>(clear_non_empty(part));
                        hint = part_opt.value() + 1;
                        continue;
                    }// end if (!mask)
                    //--------------------------
                    const IndexType base = static_cast<IndexType>(part * S_C_BITS_PER_MASK);
                    while (mask) {
                        const IndexType index = base + static_cast<uint8_t>(std::countr_zero(mask));
                        if (index >= capacity) {
                            break;
                        }// end if (index >= capacity)
                        //--------------------------
                        auto ptr = m_slots[index].load(std::memory_order_acquire);
                        if (ptr) {
                            fn(index, ptr);
                        }// end if (ptr)
                        //--------------------------
                        mask &= mask - 1;
                    }// end  while (mask)
                    //--------------------------
                    hint = part_opt.value() + 1;
                }// end for
            } // end for_each_active_fast(...)
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M > 0) and (M <= 64), bool> find_data(Fn&& fn) const {
                //--------------------------
                uint64_t mask = m_bitmask.load(std::memory_order_acquire);
                //--------------------------
                while (mask) {
                    //--------------------------
                    const uint8_t index = static_cast<uint8_t>(std::countr_zero(mask));
                    //--------------------------
                    if (index < get_capacity()) {
                        //--------------------------
                        auto ptr = m_slots[index].load(std::memory_order_acquire);
                        if (ptr and fn(ptr)) {
                            return true;
                        }// end  if (sp_data and fn(index, sp_data))
                        //--------------------------
                    }// end  if (index < get_capacity())
                    //--------------------------
                    mask &= mask - 1;
                    //--------------------------
                }// end while (mask)
                //--------------------------
                return false;
                //--------------------------
            } // end std::enable_if_t<(M > 0) and (M <= 64), bool> find_data(Fn&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M == 0) or (M > 64), bool> find_data(Fn&& fn) const {
                //--------------------------
                for (IndexType part = 0; part < get_mask_count(); ++part) {
                    //--------------------------
                    uint64_t mask        = m_bitmask[part].load(std::memory_order_acquire);
                    const IndexType base = static_cast<IndexType>(part * S_C_BITS_PER_MASK);
                    //--------------------------
                    while (mask) {
                        //--------------------------
                        const IndexType index = base + static_cast<uint8_t>(std::countr_zero(mask));
                        //--------------------------
                        if (index < get_capacity()) {
                            //--------------------------
                            auto ptr = m_slots[index].load(std::memory_order_acquire);
                            if (ptr and fn(ptr)) {
                                return true;
                            }//end if (sp_data and fn(index, sp_data))
                            //--------------------------
                        }// end if (index < get_capacity())
                        //--------------------------
                        mask &= mask - 1;
                        //--------------------------
                    }// en while (mask)
                }// end for (IndexType part = 0; part < get_mask_count(); ++part)
                //--------------------------
                return false;
                //--------------------------
            } // end std::enable_if_t<(M == 0) or (M > 64), bool> find_data(Fn&& fn) const
            //--------------------------
            void clear_data(void) {
                //--------------------------
                for_each_active_fast([this](IndexType index, T*) {
                    m_slots[index].store(nullptr, std::memory_order_release);
                });
                //--------------------------
                if constexpr ((N > 0) and (N <= 64)) {
                    m_bitmask.store(initial_bitmask(), std::memory_order_release);
                } else {
                    static_cast<void>(initialization(0ULL));
                    if (tree_enabled()) {
                        BitmapTree* tree = tree_ptr();
                        tree->reset_set(plane_index(PartPlane::Available));
                        tree->reset_clear(plane_index(PartPlane::NonEmpty));
                    }
                }// end if constexpr ((N > 0) and (N <= 64))
                //--------------------------
                m_a_size.store(0UL, std::memory_order_release);
                //--------------------------
            } // end void clear_data(void)
            //--------------------------
            IndexType size_data(void) const {
                return static_cast<IndexType>(m_a_size.load(std::memory_order_relaxed));
            } // end IndexType size_data(void) const
            //--------------------------------------------------------------
            // Helper functions
            //--------------------------------------------------------------
            bool tree_enabled(void) const noexcept {
                if constexpr (!S_C_TREE_POSSIBLE) {
                    return false;
                } else {
                    if (!m_use_tree) {
                        return false;
                    }
                    if constexpr (S_C_TREE_ALWAYS) {
                        return true;
                    } else {
                        return static_cast<bool>(m_available);
                    }
                }
            } // end bool tree_enabled(void) const noexcept
            //--------------------------
            BitmapTree* tree_ptr(void) noexcept {
                if constexpr (!S_C_TREE_POSSIBLE) {
                    return nullptr;
                } else {
                    if constexpr (S_C_TREE_ALWAYS) {
                        return &m_available;
                    } else {
                        return m_available.get();
                    }
                }
            } // end BitmapTree* tree_ptr(void) noexcept
            //--------------------------
            BitmapTree* tree_ptr(void) const noexcept {
                if constexpr (!S_C_TREE_POSSIBLE) {
                    return nullptr;
                } else {
                    if constexpr (S_C_TREE_ALWAYS) {
                        return &m_available;
                    } else {
                        return m_available.get();
                    }
                }
            } // end BitmapTree* tree_ptr(void) const noexcept
            //--------------------------
            void disable_tree(void) noexcept {
                if constexpr (!S_C_TREE_POSSIBLE) {
                    return;
                } else {
                    m_use_tree = false;
                    if constexpr (S_C_TREE_ALWAYS) {
                        m_available = BitmapTree();
                    } else {
                        m_available.reset();
                    }
                }
            } // end void disable_tree(void) noexcept
            //--------------------------
            uint8_t select_free_bit(const uint64_t& mask, const uint8_t& _bit_hint) noexcept {
                //--------------------------
                const uint64_t _free = ~mask;
                //--------------------------
                if constexpr (S_C_ENABLE_ROTATION) {
                    if ((_bit_hint != 0) and (std::popcount(_free) >= S_C_ROTATE_THRESHOLD)) {
                        //--------------------------
                        const uint64_t _rotated   = std::rotr(_free, _bit_hint);
                        const uint8_t _bit_offset = static_cast<uint8_t>(std::countr_zero(_rotated));
                        uint16_t _bit             = static_cast<uint16_t>(_bit_offset + _bit_hint);
                        //--------------------------
                        if (_bit >= S_C_BITS_PER_MASK) {
                            _bit = static_cast<uint16_t>(_bit - S_C_BITS_PER_MASK);
                        }// end if (_bit >= C_BITS_PER_MASK)
                        //--------------------------
                        return static_cast<uint8_t>(_bit);
                    }// end if ((bit_hint != 0) and (std::popcount(free) >= C_ROTATE_THRESHOLD))
                } else {
                    static_cast<void>(_bit_hint);
                }// end if constexpr (C_ENABLE_ROTATION)
                return static_cast<uint8_t>(std::countr_zero(_free));
            } // end uint8_t select_free_bit(const uint64_t& mask, const uint8_t& _bit_hint) noexcept
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), std::optional<size_t>>
            scan_available(const size_t& _start_part, const size_t& _c_mask_count_size, const size_t& _c_available_plane) {
                //--------------------------
                const bool _use_tree = tree_enabled();
                BitmapTree* tree     = _use_tree ? tree_ptr() : nullptr;
                for (size_t offset = 0; offset < _c_mask_count_size; ++offset) {
                    //--------------------------
                    size_t probe = _start_part + offset;
                    //--------------------------
                    if (probe >= _c_mask_count_size) {
                        probe -= _c_mask_count_size;
                    }// end if (probe >= mask_count_size)
                    //--------------------------
                    if (m_bitmask[probe].load(std::memory_order_acquire) != ~0ULL) {
                        if (_use_tree) {
                            tree->set(probe, _c_available_plane);
                        }// end if (_use_tree)
                        return probe;
                    }// end if (m_bitmask[probe].load(std::memory_order_acquire) != ~0ULL)
                }// end for (size_t offset = 0; offset < mask_count_size; ++offset)
                return std::nullopt;
            } // end scan_available(...)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool>
            refresh_hint(const IndexType& part, const size_t& _c_available_plane) noexcept {
                //--------------------------
                if (!tree_enabled()) {
                    return false;
                }// end if (!tree_enabled)
                //--------------------------
                BitmapTree* tree = tree_ptr();
                tree->clear(static_cast<size_t>(part), _c_available_plane);
                if (m_bitmask[part].load(std::memory_order_acquire) != ~0ULL) {
                    tree->set(static_cast<size_t>(part), _c_available_plane);
                }// end if (m_bitmask[part].load(std::memory_order_acquire) != ~0ULL)
                //--------------------------
                return true;
            } // end refresh_hint(...)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool>
            update_on_full(const IndexType& part, const uint64_t& desired, const size_t& _c_available_plane) noexcept {
                if (desired != ~0ULL) {
                    return true;
                }// end if (desired != ~0ULL)
                return refresh_hint(part, _c_available_plane);
            } // end update_on_full(...)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool>
            available_not_full(const IndexType& part, const uint64_t& _c_old, const size_t& _c_available_plane) noexcept {
                //--------------------------
                if (_c_old != ~0ULL) {
                    return true;
                }// end if (old != ~0ULL)
                //--------------------------
                if (!tree_enabled()) {
                    return false;
                }// end if (!tree_enabled)
                //--------------------------
                return tree_ptr()->set(static_cast<size_t>(part), _c_available_plane);
            } // end available_not_full(...)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool> mark_non_empty(IndexType part) noexcept {
                if (!tree_enabled()) {
                    return false;
                }// end if (!tree_enabled)
                return tree_ptr()->set(static_cast<size_t>(part), plane_index(PartPlane::NonEmpty));
            } // end std::enable_if_t<(M == 0) or (M > 64), bool> mark_non_empty(IndexType part) noexcept
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool> clear_non_empty(IndexType part) const noexcept {
                if (!tree_enabled()) {
                    return false;
                }// end if (!tree_enabled)
                return tree_ptr()->clear(static_cast<size_t>(part), plane_index(PartPlane::NonEmpty));
            } // end std::enable_if_t<(M == 0) or (M > 64), bool> clear_non_empty(IndexType part) const noexcept
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M > 0) and (M <= 64), bool> reacquire_index(const IndexType& index) {
                //--------------------------
                const uint64_t bit = 1ULL << index;
                uint64_t mask      = m_bitmask.load(std::memory_order_relaxed);
                //--------------------------
                while ((mask & bit) == 0) {
                    const uint64_t desired = mask | bit;
                    if (m_bitmask.compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)) {
                        m_a_size.fetch_add(1, std::memory_order_relaxed);
                        return true;
                    }// end if (m_bitmask.compare_exchange_weak(...))
                }// end while ((mask & bit) == 0)
                //--------------------------
                return false;
                //--------------------------
            } // end std::enable_if_t<(M > 0) and (M <= 64), bool> reacquire_index(const IndexType& index)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool> reacquire_index(const IndexType& index) {
                //--------------------------
                const IndexType part = part_index(index);
                const uint16_t bit   = bit_index(index);
                const uint64_t flag  = 1ULL << bit;
                //--------------------------
                uint64_t mask = m_bitmask[part].load(std::memory_order_relaxed);
                //--------------------------
                while ((mask & flag) == 0) {
                    const uint64_t desired = mask | flag;
                    if (m_bitmask[part].compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)) {
                        m_a_size.fetch_add(1, std::memory_order_relaxed);
                        const bool marked = mark_non_empty(part);
                        if (marked) {
                            static_cast<void>(update_on_full(part, desired, plane_index(PartPlane::Available)));
                        }
                        return true;
                    }// end if (m_bitmask[part].compare_exchange_weak(...))
                }// end while ((mask & flag) == 0)
                //--------------------------
                return false;
                //--------------------------
            } // end std::enable_if_t<(M == 0) or (M > 64), bool> reacquire_index(const IndexType& index)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M > 64) or (M == 0), bool>
            invalid_bits(const IndexType& capacity, const IndexType& _c_mask_count) {
                //--------------------------
                if (!(capacity and _c_mask_count)) {
                    return false;
                }// end if (!(capacity and mask_count))
                //--------------------------
                const IndexType valid_bits = capacity - static_cast<IndexType>((_c_mask_count - 1) * S_C_BITS_PER_MASK);
                if (valid_bits < S_C_BITS_PER_MASK) {
                    //--------------------------
                    const uint64_t valid_mask   = (valid_bits == 0) ? 0ULL : ((1ULL << valid_bits) - 1ULL);
                    const uint64_t invalid_mask = ~valid_mask;
                    //--------------------------
                    m_bitmask[_c_mask_count - 1].fetch_or(invalid_mask, std::memory_order_relaxed);
                    //--------------------------
                }// end if (valid_bits < C_BITS_PER_MASK)
                //--------------------------
                return true;
            } // end or(...)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M > 64) or (M == 0), bool> initialization(uint64_t value) {
                //--------------------------
                for (auto& mask : m_bitmask) {
                    mask.store(value, std::memory_order_relaxed);
                }// end for (auto& mask : m_bitmask)
                //--------------------------
                // Mark out-of-capacity bits as permanently unavailable so full masks become ~0ULL.
                const IndexType capacity      = get_capacity();
                const IndexType _c_mask_count = get_mask_count();
                //--------------------------
                if(!invalid_bits(capacity, _c_mask_count)){
                    return false;
                }// end if(!invalid_bits(capacity, mask_count))
                //--------------------------
                return true;
                //--------------------------
            } // end std::enable_if_t<(M > 64) or (M == 0), bool> initialization(uint64_t value)
            //--------------------------
            bool maybe_initialize_tree(const size_t& leaf_bits) {
                //--------------------------
                if constexpr (!S_C_TREE_POSSIBLE) {
                    return true;
                } else {
                    if (!m_use_tree) {
                        return true;
                    }// end if (!m_use_tree)
                    //--------------------------
                    return initialize_tree(leaf_bits);
                }
            } // end bool maybe_initialize_tree(const size_t& leaf_bits)
            //--------------------------
            bool initialize_tree(const size_t& leaf_bits) {
                //--------------------------
                if constexpr (!S_C_TREE_POSSIBLE) {
                    return true;
                } else {
                    if (!leaf_bits) {
                        disable_tree();
                        return false;
                    }// end if (!leaf_bits)
                    //--------------------------
                    if constexpr (!S_C_TREE_ALWAYS) {
                        if (!m_available) {
                            m_available = std::make_unique<BitmapTree>();
                        }// end if (!m_available)
                    }
                    //--------------------------
                    BitmapTree* tree = tree_ptr();
                    if (!tree or !tree->initialization(leaf_bits, plane_count())) {
                        disable_tree();
                        return false;
                    }// end if (!tree or !tree->initialization(leaf_bits, plane_count()))
                    //--------------------------
                    return tree->reset_set(plane_index(PartPlane::Available)) and tree->reset_clear(plane_index(PartPlane::NonEmpty));
                }
            } // end bool initialize_tree(const size_t& leaf_bits)
            //--------------------------------------------------------------
            // Constexpr / Consteval helpers
            //--------------------------------------------------------------
            constexpr IndexType get_capacity(void) const {
                //--------------------------
                if constexpr ((N == 0) or (N > S_C_ARRAY_LIMIT)) {
                    return m_a_capacity.load(std::memory_order_relaxed);
                }// end if constexpr ((N == 0) or (N > C_ARRAY_LIMIT))
                //--------------------------
                return N;
                //--------------------------
            } // end constexpr IndexType get_capacity(void) const
            //--------------------------
            constexpr IndexType get_mask_count(void) const {
                //--------------------------
                if constexpr ((N == 0) or (N > S_C_ARRAY_LIMIT)) {
                    return m_a_mask_count.load(std::memory_order_relaxed);
                }// end if constexpr ((N == 0) or (N > C_ARRAY_LIMIT))
                //--------------------------
                return S_C_MASK_COUNT;
                //--------------------------
            } // end constexpr IndexType get_mask_count(void) const
            //--------------------------
            constexpr IndexType part_index(IndexType index) const noexcept {
                return static_cast<IndexType>(index / S_C_BITS_PER_MASK);
            } // end constexpr IndexType part_index(IndexType index) const noexcept
            //--------------------------
            constexpr uint16_t bit_index(IndexType index) const noexcept {
                return static_cast<uint16_t>(index % S_C_BITS_PER_MASK);
            } // end constexpr uint16_t bit_index(IndexType index) const noexcept
            //--------------------------
            constexpr size_t bitmask_calculator(size_t capacity) noexcept {
                return (capacity) ? static_cast<size_t>((capacity + S_C_BITS_PER_MASK - 1) / S_C_BITS_PER_MASK) : 0UL;
            } // end constexpr size_t bitmask_calculator(size_t capacity) noexcept
            //--------------------------
            constexpr size_t bitmask_capacity(size_t capacity) noexcept {
                return std::bit_ceil(capacity);
            } // end constexpr size_t bitmask_capacity(size_t capacity) noexcept
            //--------------------------
            constexpr bool use_tree(const size_t& capacity) const noexcept {
                return capacity > static_cast<size_t>(S_C_ARRAY_LIMIT);
            } // end constexpr bool use_tree(const size_t& capacity) const noexcept
            //--------------------------
            constexpr size_t plane_index(PartPlane plane) const noexcept {
                return static_cast<size_t>(plane);
            } // end constexpr size_t plane_index(PartPlane plane) const noexcept
            //--------------------------
            constexpr size_t plane_count(void) const noexcept {
                return plane_index(PartPlane::Count);
            } // end constexpr size_t plane_count(void) const noexcept
            //--------------------------
            constexpr uint64_t initial_bitmask(void) const noexcept {
                if constexpr ((N > 0) and (N < S_C_BITS_PER_MASK)) {
                    return ~((1ULL << N) - 1ULL);
                }// end if constexpr ((N > 0) and (N < C_BITS_PER_MASK))
                return 0ULL;
            } // end constexpr uint64_t initial_bitmask(void) const noexcept
            //--------------------------------------------------------------
        private:
            //--------------------------------------------------------------
            std::atomic<size_t> m_a_capacity, m_a_mask_count, m_a_size;
            //--------------------------
            using BitmaskType = std::conditional_t<(N == 0) or (N > S_C_ARRAY_LIMIT), std::vector<std::atomic<uint64_t>>,
                                    std::conditional_t<(N > S_C_BITS_PER_MASK) and (N <= S_C_ARRAY_LIMIT ), std::array<std::atomic<uint64_t>, S_C_MASK_COUNT>,
                                    std::atomic<uint64_t>>>;
            //--------------------------
            SlotType m_slots;
            BitmaskType m_bitmask;
            mutable TreeStorage m_available;
            bool m_use_tree;
            const bool m_c_initialize;
        //--------------------------------------------------------------
    }; // end class BitmaskTable
    //--------------------------------------------------------------
} // end namespace HazardSystem
//--------------------------------------------------------------
