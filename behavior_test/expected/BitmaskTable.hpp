#pragma once
//--------------------------------------------------------------
// Standard Cpp Libraries
//--------------------------------------------------------------
#include <array>
#include <atomic>
#include <bit>
#include <cstddef>
#include <cstdint>
#include <limits>
#include <memory>
#include <optional>
#include <type_traits>
#include <utility>
#include <vector>

//--------------------------------------------------------------
// User Defined Headers
//--------------------------------------------------------------
#include "BitmapTree.hpp"
#include "HazardPointer.hpp"
namespace HazardSystem {
    //--------------------------------------------------------------
    template<typename T, uint16_t N = 0>
    class BitmaskTable {
        //--------------------------------------------------------------
        private:
            //--------------------------------------------------------------
#if defined(BUILD_HAZARDSYSTEM_DISABLE_BITMASK_ROTATION)
            static constexpr bool C_ENABLE_ROTATION     = false;
#else
            static constexpr bool C_ENABLE_ROTATION     = true;
#endif
            //--------------------------
            static constexpr uint16_t S_C_ARRAY_LIMIT     = 1024U;
            //--------------------------
            static constexpr uint16_t C_BITS_PER_MASK   = std::numeric_limits<uint64_t>::digits;

            static constexpr uint8_t S_C_ROTATE_THRESHOLD = static_cast<uint8_t>(C_BITS_PER_MASK / 2);
            static constexpr uint16_t S_C_MASK_COUNT      = (N == 0 ? 0 : static_cast<uint16_t>((N + C_BITS_PER_MASK - 1) / C_BITS_PER_MASK));
            //--------------------------
            static constexpr bool C_TREE_POSSIBLE = (N == 0) or (N > S_C_ARRAY_LIMIT);
            static constexpr bool C_TREE_ALWAYS   = (N > S_C_ARRAY_LIMIT);
            //--------------------------
            struct NoTree {
            }; // struct NoTree
            //--------------------------
            using TreeStorage                            = std::conditional_t<C_TREE_POSSIBLE,
                                                            std::conditional_t<C_TREE_ALWAYS, BitmapTree, std::unique_ptr<BitmapTree>>,
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
            };// end struct IndexTypeSelector<M, true>
            //--------------------------
            template<uint16_t M>
            struct IndexTypeSelector<M, false> {
                using type = std::conditional_t<(M <= std::numeric_limits<uint8_t>::max()), uint8_t, uint16_t>;
            };// end struct IndexTypeSelector<M, false>
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
            BitmaskTable(void) :    m_capacity(0UL),
                                    m_mask_count(0UL),
                                    m_size(0UL),
                                    m_slots(),
                                    m_bitmask(),
                                    m_available(),
                                    m_use_tree(false),
                                    m_c_initialize(false) {
                //--------------------------
            }// end BitmaskTable(void)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t<(M > 0) and (M <= 64), int> = 0>
            BitmaskTable(void) :    m_capacity(0UL),
                                    m_mask_count(0UL),
                                    m_size(0UL),
                                    m_slots(),
                                    m_bitmask(initial_bitmask()),
                                    m_available(),
                                    m_use_tree(false),
                                    m_c_initialize(false) {
                //--------------------------
            }// end BitmaskTable(void)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t< (M > 64) and (M <= S_C_ARRAY_LIMIT), int> = 0>
            BitmaskTable(void) :    m_capacity(0UL),
                                    m_mask_count(0UL),
                                    m_size(0UL),
                                    m_slots(),
                                    m_bitmask(),
                                    m_available(),
                                    m_use_tree(false),
                                    m_c_initialize(initialization(0ULL) and maybe_initialize_tree(static_cast<size_t>(get_mask_count()))) {
                //--------------------------
            }// end BitmaskTable(void)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t< (M == 0), int> = 0>
            BitmaskTable(const size_t& capacity) :  m_capacity(bitmask_capacity(capacity)),
                                                    m_mask_count(bitmask_calculator(bitmask_capacity(capacity))),
                                                    m_size(0UL),
                                                    m_slots(bitmask_capacity(capacity)),
                                                    m_bitmask(bitmask_calculator(bitmask_capacity(capacity))),
                                                    m_available(),
                                                    m_use_tree(use_tree(bitmask_capacity(capacity))),
                                                    m_c_initialize(initialization(0ULL) and maybe_initialize_tree(static_cast<size_t>(get_mask_count()))) {
                //--------------------------
            }// end BitmaskTable(const size_t& capacity)
            //--------------------------
            template <uint16_t M = N, std::enable_if_t< (M > S_C_ARRAY_LIMIT), int> = 0>
            BitmaskTable(void) :    m_capacity(bitmask_capacity(N)),
                                    m_mask_count(bitmask_calculator(bitmask_capacity(N))),
                                    m_size(0UL),
                                    m_slots(bitmask_capacity(N)),
                                    m_bitmask(bitmask_calculator(bitmask_capacity(N))),
                                    m_available(),
                                    m_use_tree(use_tree(bitmask_capacity(N))),
                                    m_c_initialize(initialization(0ULL) and maybe_initialize_tree(static_cast<size_t>(get_mask_count()))) {
                //--------------------------
            }// end BitmaskTable(const size_t& capacity)
            //--------------------------
            ~BitmaskTable(void)                           = default;
            //--------------------------
            BitmaskTable(const BitmaskTable&)            = delete;
            BitmaskTable& operator=(const BitmaskTable&) = delete;
            BitmaskTable(BitmaskTable&&)                 = default;
            BitmaskTable& operator=(BitmaskTable&&)      = default;
            //--------------------------------------------------------------
        public:
            //--------------------------------------------------------------
            std::optional<IndexType> acquire(void) {
                return acquire_data();
            }// end std::optional<IndexType> acquire_data(void)
            //--------------------------
            std::optional<iterator> acquire_iterator(void) {
                return acquire_data_iterator();
            }// end acquire_iterator
            //--------------------------
            std::optional<const_iterator> acquire_iterator(void) const {
                return acquire_data_iterator();
            }// std::optional<const_iterator> acquire_data_iterator(void) const
            //--------------------------
            bool acquire(iterator it) {
                return reacquire_iterator(it);
            }// end bool try_acquire(iterator it)
            //--------------------------
            bool release(const IndexType& index) {
                return release_data(index);
            }// end bool release(const IndexType& index)
            //--------------------------
            bool release(const std::optional<IndexType>& index) {
                //--------------------------
                if(!index.has_value()) {
                    return false;
                }// end if(!index.has_value())
                //--------------------------
                return release_data(index.value());
                //--------------------------
            }// end bool release(const std::optional<IndexType>& index)
            //--------------------------
            bool set(const IndexType& index, T* _ptr) {
                return set_data(index, _ptr);
            }// end bool set(const IndexType& index, T* ptr)
            //--------------------------
            bool set(const std::optional<IndexType>& index, T* _ptr) {
                //--------------------------
                if(!index.has_value()) {
                    return false;
                }// end if(!index.has_value())
                //--------------------------
                return set_data(index.value(), _ptr);
                //--------------------------
            }// end bool set(const std::optional<IndexType>& index, T* ptr)
            //--------------------------
            std::optional<IndexType> set(T* _ptr) {
                return set_data(_ptr);
            }// end std::optional<IndexType> data(T* ptr)
            //--------------------------
            bool set(const_iterator it, T* _ptr) {
                return set_data(it, _ptr);
            }// end bool set(const_iterator it, T* ptr)
            //--------------------------
            T* at(const IndexType& index) const {
                return at_data(index);
            }// end std::optional<T*> at_data(const IndexType& index) const
            //--------------------------
            T* at(const std::optional<IndexType>& index) const {
                //--------------------------
                if(!index.has_value()) {
                    return nullptr;
                }// end if(!index.has_value())
                //--------------------------
                return at_data(index.value());
                //--------------------------
            }// end std::optional<T*> at_data(const std::optional<IndexType>& index) const
            //--------------------------
            bool active(const IndexType& index) const {
                return active_data(index);
            }// end bool active(const IndexType& index) const
            //--------------------------
            bool active(const std::optional<IndexType>& index) const {
                //--------------------------
                if(!index.has_value()) {
                    return false;
                }// end if(!index.has_value())
                //--------------------------
                return active_data(index.value());
                //--------------------------
            }// end bool active(const std::optional<IndexType>& index) const
            //--------------------------
            template <typename Fn>
            void for_each(Fn&& _t_fn) const {
                for_each_active(std::forward<Fn>(_t_fn));
            }// end void for_each(...)
            //--------------------------
            template <typename Fn>
            void for_each_fast(Fn&& _t_fn) const {
                for_each_active_fast(std::forward<Fn>(_t_fn));
            }// end void for_each_fast(...)
            //--------------------------
            template <typename Fn>
            bool find(Fn&& _t_fn) const {
                return find_data(std::forward<Fn>(_t_fn));
            }// end bool find(...)
            //--------------------------
            void clear(void) {
                clear_data();
            }// end void clear(void)
            //--------------------------
            IndexType size(void) const {
                return size_data();
            }// end IndexType size_data(void) const
            //--------------------------
            constexpr IndexType capacity(void) const {
                return get_capacity();
            }// end constexpr uint16_t capacity(void) const
            //--------------------------
            iterator begin(void) noexcept {
                return m_slots.begin();
            }// end iterator begin(void) noexcept
            //--------------------------
            iterator end(void) noexcept {
                return m_slots.end();
            }// end iterator end(void) noexcept
            //--------------------------
            const_iterator begin(void) const noexcept {
                return m_slots.begin();
            }// end const_iterator begin(void) const noexcept
            //--------------------------
            const_iterator end(void) const noexcept {
                return m_slots.end();
            }// end const_iterator end(void) const noexcept
            //--------------------------
            const_iterator cbegin(void) const noexcept {
                return m_slots.cbegin();
            }//end const_iterator cbegin(void) const noexcept
            //--------------------------
            const_iterator cend(void) const noexcept {
                return m_slots.cend();
            }//end const_iterator cend(void) const noexcept
            //--------------------------
            reverse_iterator rbegin(void) noexcept {
                return m_slots.rbegin();
            }//end reverse_iterator rbegin(void) noexcept
            //--------------------------
            reverse_iterator rend(void) noexcept {
                return m_slots.rend();
            }//end reverse_iterator rend(void) noexcept
            //--------------------------
            const_reverse_iterator rbegin(void) const noexcept {
                return m_slots.rbegin();
            }//end const_reverse_iterator rbegin(void) const
            //--------------------------
            const_reverse_iterator rend(void) const noexcept {
                return m_slots.rend();
            }//end const_reverse_iterator rend(void) const noexcept
            //--------------------------
            const_reverse_iterator crbegin(void) const noexcept {
                return m_slots.crbegin();
            }//end const_reverse_iterator crbegin(void) const noexcept
            //--------------------------
            const_reverse_iterator crend(void) const noexcept {
                return m_slots.crend();
            }//end const_reverse_iterator crend(void) const noexcept
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
                        m_size.fetch_add(1, std::memory_order_relaxed);
                        return index;
                    }// end if (m_bitmask.compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)))
                }// end while (mask != ~0ULL)
                //--------------------------
                return std::nullopt;
                //--------------------------
            }// end std::enable_if_t<(M > 0) && (M <= 64), std::optional<IndexType>> acquire_data(void)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), std::optional<IndexType>> acquire_data(void) {
                //--------------------------
                const IndexType capacity        = get_capacity();
                const IndexType _c_mask_count   = get_mask_count();
                const size_t _c_capacity_size   = static_cast<size_t>(capacity);
                const size_t _c_mask_count_size = static_cast<size_t>(_c_mask_count);
                //--------------------------
                if (!capacity or !_c_mask_count) {
                    return std::nullopt;
                }// end if (!capacity or !mask_count)
                //--------------------------
                const size_t _c_available_plane = plane_index(PartPlane::Available);
                const bool _c_use_tree          = tree_enabled();
                BitmapTree* _p_tree             = _c_use_tree ? tree_ptr() : nullptr;
                thread_local size_t _part_hint  = 0;
                thread_local uint8_t _bit_hint  = 0;
                size_t _start_part              = _part_hint % _c_mask_count_size;
                //--------------------------
                while (m_size.load(std::memory_order_relaxed) < _c_capacity_size) {
                    std::optional<size_t> _part_opt;
                    if (_c_use_tree) {
                        _part_opt = _p_tree->find(_start_part, _c_available_plane);
                        if (!_part_opt) {
                            // Tree is a hint; fall back to a bounded scan to avoid spurious failures under contention.
                            //--------------------------
                            if (m_size.load(std::memory_order_relaxed) >= _c_capacity_size) {
                                return std::nullopt;
                            }// end if (m_size.load(std::memory_order_relaxed) >= capacity)
                            //--------------------------
                            _part_opt = scan_available(_start_part, _c_mask_count_size, _c_available_plane);
                            if (!_part_opt) {
                                return std::nullopt;
                            }// end if (!part_opt)
                        }// end if (!part_opt)
                    } else {
                        _part_opt = scan_available(_start_part, _c_mask_count_size, _c_available_plane);
                        if (!_part_opt) {
                            return std::nullopt;
                        }// end if (!part_opt)
                    }// end if (_use_tree)
                    //--------------------------
                    const IndexType part = static_cast<IndexType>(_part_opt.value());
                    _part_hint           = static_cast<size_t>(part);
                    _start_part          = (static_cast<size_t>(part) + 1) % _c_mask_count_size;
                    uint64_t mask        = m_bitmask[part].load(std::memory_order_relaxed);
                    //--------------------------
                    while (mask != ~0ULL) {
                        //--------------------------
                        const uint8_t bit             = select_free_bit(mask, _bit_hint);
                        const IndexType _c_slot_index = static_cast<IndexType>((part * C_BITS_PER_MASK) + bit);
                        //--------------------------
                        if (_c_slot_index >= capacity) {
                            break;
                        }// end if (slot_index >= capacity)
                        //--------------------------
                        const uint64_t flag    = 1ULL << bit;
                        const uint64_t desired = mask | flag;
                        //--------------------------
                        if (m_bitmask[part].compare_exchange_weak(mask, desired, std::memory_order_acq_rel, std::memory_order_relaxed)) {
                            m_size.fetch_add(1, std::memory_order_relaxed);
                            static_cast<void>(mark_non_empty(part));
                            if constexpr (C_ENABLE_ROTATION) {
                                _bit_hint = static_cast<uint8_t>((bit + 1) % C_BITS_PER_MASK);
                            }// end if constexpr (C_ENABLE_ROTATION)
                            static_cast<void>(update_on_full(part, desired, _c_available_plane));
                            return _c_slot_index;
                        }// end if (m_bitmask[part].compare_exchange_weak(...))
                    }// end while (mask != ~0ULL)
                    //--------------------------
                    // Part is (now) full; clear and retry.
                    static_cast<void>(refresh_hint(part, _c_available_plane));
                }// end while (m_size.load(std::memory_order_relaxed) < capacity)
                return std::nullopt;
                //--------------------------
            }// end std::enable_if_t<(M == 0) or (M > 64), std::optional<IndexType>> acquire_data(void)
            //--------------------------
            std::optional<iterator> acquire_data_iterator(void) {
                //--------------------------
                auto _c_index = acquire_data();
                if (!_c_index) {
                    return std::nullopt;
                }// end if (!_index)
                //--------------------------
                return m_slots.begin() + static_cast<typename SlotType::difference_type>(_c_index.value());
                //--------------------------
            }// end std::optional<iterator> acquire_data_iterator(void)
            //--------------------------
            std::optional<const_iterator> acquire_data_iterator(void) const {
                //--------------------------
                auto _c_index = acquire_data();
                if (!_c_index) {
                    return std::nullopt;
                }// end if (!_index)
                //--------------------------
                return m_slots.begin() + static_cast<typename SlotType::difference_type>(_c_index.value());
                //--------------------------
            }// end std::optional<const_iterator> acquire_data_iterator(void) const
            //--------------------------
            bool reacquire_iterator(const_iterator it) {
                //--------------------------
                const auto first   = m_slots.begin();
                const auto _c_last = m_slots.end();
                //--------------------------
                if (it < first or it >= _c_last) {
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
            }// end bool reacquire_iterator(const_iterator it)
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
                m_size.fetch_sub(1, std::memory_order_relaxed);
                return true;
                //--------------------------
            }// end bool release_data(const IndexType& index)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M > 0) and (M <= 64) , bool> set_data(const IndexType& index, T* _ptr) {
                //--------------------------
                if (index >= get_capacity()) {
                    return false;
                }// end if (index >= get_capacity())
                //--------------------------
                m_slots[index].store(_ptr, std::memory_order_release);
                //--------------------------
                const uint64_t bit = 1ULL << index;
                //--------------------------
                if (_ptr) {
                    const uint64_t _c_old = m_bitmask.fetch_or(bit, std::memory_order_acq_rel);
                    if ((_c_old & bit) == 0) {
                        m_size.fetch_add(1, std::memory_order_relaxed);
                    }// end if ((old & bit) == 0)
                } else {
                    const uint64_t _c_old = m_bitmask.fetch_and(~bit, std::memory_order_acq_rel);
                    if (_c_old & bit) {
                        m_size.fetch_sub(1, std::memory_order_relaxed);
                    }// end if (old & bit)
                }// end  if (ptr)
                //--------------------------
                return true;
                //--------------------------
            }// end std::enable_if_t<(M <= 64), bool> set_data(const IndexType& index, T* ptr)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool> set_data(const IndexType& index, T* _ptr) {
                //--------------------------
                if (index >= get_capacity()) {
                    return false;
                }// end if (index >= get_capacity())
                //--------------------------
                m_slots[index].store(_ptr, std::memory_order_release);
                //--------------------------
                const IndexType part      = part_index(index);
                const uint16_t bit        = bit_index(index);
                const uint64_t _c_bitmask = 1ULL << bit;
                //--------------------------
                if (_ptr) {
                    //--------------------------
                    const uint64_t _c_old = m_bitmask[part].fetch_or(_c_bitmask, std::memory_order_acq_rel);
                    const bool _c_marked  = mark_non_empty(part);
                    //--------------------------
                    if ((_c_old & _c_bitmask) == 0) {
                        m_size.fetch_add(1, std::memory_order_relaxed);
                    }// end if ((old & bitmask) == 0)
                    const uint64_t _c_now = _c_old | _c_bitmask;
                    if (_c_marked) {
                        static_cast<void>(update_on_full(part, _c_now, plane_index(PartPlane::Available)));
                    }
                } else {
                    const uint64_t _c_old = m_bitmask[part].fetch_and(~_c_bitmask, std::memory_order_acq_rel);
                    if (_c_old & _c_bitmask) {
                        m_size.fetch_sub(1, std::memory_order_relaxed);
                    }// end if (old & bitmask)
                    static_cast<void>(available_not_full(part, _c_old, plane_index(PartPlane::Available)));
                }// end if (ptr)
                //--------------------------
                return true;
                //--------------------------
            }// end std::enable_if_t<(M > 64), bool> set_data(const IndexType& index, T* ptr)
            //--------------------------
            std::optional<IndexType> set_data(T* _ptr) {
                //--------------------------
                if (!_ptr) {
                    return std::nullopt;
                }// end if (!ptr)
                //--------------------------
                std::optional<IndexType> _c_index = acquire_data();
                //--------------------------
                if (!_c_index) {
                    return std::nullopt;
                }// end if (!_index)
                //--------------------------
                set_data(_c_index.value(), _ptr);
                //--------------------------
                return _c_index;
                //--------------------------
            }// end std::optional<IndexType> set_data(T* ptr)
            //--------------------------
            bool set_data(const_iterator it, T* _ptr) {
                //--------------------------
                auto first = m_slots.begin();
                //--------------------------
                if (it < first or it >= m_slots.end()) {
                    return false;
                }// end if (it < first or it >= m_slots.end())
                //--------------------------
                return set_data(static_cast<IndexType>(it - first), _ptr);
                //--------------------------
            }// end bool set_data(iterator it, T* ptr)
            //--------------------------
            T* at_data(const IndexType& index) const {
                //--------------------------
                if (index >= get_capacity()) {
                    return nullptr;
                }// end if (index >= get_capacity())
                //--------------------------
                return m_slots[index].load(std::memory_order_acquire);
                //--------------------------
            }// end std::optional<T*> at_data(const IndexType& index) const
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
            }// end bool active_data(const IndexType& index) const
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
            }// end uint16_t active_count_data(void) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M > 0) and (M <= 64), void> for_each_active(Fn&& _t_fn) const {
                //--------------------------
                const uint64_t mask = m_bitmask.load(std::memory_order_acquire);
                //--------------------------
                for (IndexType index = 0; index < N; ++index) {
                    //--------------------------
                    if (mask & (1ULL << index)) {
                        auto _ptr = m_slots[index].load(std::memory_order_acquire);
                        if (_ptr) {
                            _t_fn(index, _ptr);
                        }// end if (ptr)
                    }// end if (mask & (1ULL << index))
                    //--------------------------
                }// end for (IndexType index = 0; index < N; ++index)
                //--------------------------
            }// end void for_each_active(std::function<void(IndexType index, T*)>&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M == 0) or (M > 64), void> for_each_active(Fn&& _t_fn) const {
                //--------------------------
                for (IndexType part = 0; part < get_mask_count(); ++part) {
                    //--------------------------
                    const uint64_t mask     = m_bitmask[part].load(std::memory_order_acquire);
                    const IndexType _c_base = static_cast<IndexType>(part * C_BITS_PER_MASK);
                    //--------------------------
                    for (uint8_t bit = 0; bit < C_BITS_PER_MASK; ++bit) {
                        //--------------------------
                        IndexType index = _c_base + bit;
                        if (index >= get_capacity()) {
                            break;
                        }// end if (index >= get_capacity())
                        //--------------------------
                        if (mask & (1ULL << bit)) {
                            auto _ptr = m_slots[index].load(std::memory_order_acquire);
                            if (_ptr) {
                                _t_fn(index, _ptr);
                            }// end if (ptr)
                        }// end if (mask & (1ULL << bit))
                        //--------------------------
                    }// end for (uint8_t bit = 0; bit < C_BITS_PER_MASK; ++bit)
                }// end for (uint16_t part = 0; part < C_MASK_COUNT; ++part)
            }// end void for_each_active(std::function<void(IndexType index, T*)>&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M > 0) and (M <= 64), void> for_each_active_fast(Fn&& _t_fn) const {
                //--------------------------
                uint64_t mask = m_bitmask.load(std::memory_order_acquire);
                //--------------------------
                while (mask) {
                    //--------------------------
                    const uint8_t _c_index = static_cast<uint8_t>(std::countr_zero(mask));
                    //--------------------------
                    if (_c_index < get_capacity()) {
                        auto _ptr = m_slots[_c_index].load(std::memory_order_acquire);
                        if (_ptr) {
                            _t_fn(_c_index, _ptr);
                        }// end if (ptr)
                    }// end if (_index < get_capacity())
                    //--------------------------
                    mask &= mask - 1; // Clear the lowest set bit
                    //--------------------------
                }// end while (mask)
                //--------------------------
            }// end void for_each_active_fast(std::function<void(IndexType index, T*)>&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M == 0) or (M > 64), void> for_each_active_fast(Fn&& _t_fn) const {
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
                        const IndexType _c_base = static_cast<IndexType>(part * C_BITS_PER_MASK);
                        while (mask) {
                            const IndexType index = _c_base + static_cast<uint8_t>(std::countr_zero(mask));
                            if (index >= capacity) {
                                break;
                            }// end if (index >= capacity)
                            //--------------------------
                            auto _ptr = m_slots[index].load(std::memory_order_acquire);
                            if (_ptr) {
                                _t_fn(index, _ptr);
                            }// end if (ptr)
                            //--------------------------
                            mask &= mask - 1;
                        }// end  while (mask)
                    }// end for (IndexType part = 0; part < mask_count; ++part)
                    return;
                }// end if (!tree_enabled)
                //--------------------------
                size_t _hint        = 0;
                BitmapTree* _p_tree = tree_ptr();
                for (auto _part_opt = _p_tree->find_next(_hint, plane_index(PartPlane::NonEmpty));
                        _part_opt;
                        _part_opt = _p_tree->find_next(_hint, plane_index(PartPlane::NonEmpty))) {
                    //--------------------------
                    const IndexType part = static_cast<IndexType>(_part_opt.value());
                    uint64_t mask        = m_bitmask[part].load(std::memory_order_acquire);
                    //--------------------------
                    if (!mask) {
                        static_cast<void>(clear_non_empty(part));
                        _hint = _part_opt.value() + 1;
                        continue;
                    }// end if (!mask)
                    //--------------------------
                    const IndexType _c_base = static_cast<IndexType>(part * C_BITS_PER_MASK);
                    while (mask) {
                        const IndexType index = _c_base + static_cast<uint8_t>(std::countr_zero(mask));
                        if (index >= capacity) {
                            break;
                        }// end if (index >= capacity)
                        //--------------------------
                        auto _ptr = m_slots[index].load(std::memory_order_acquire);
                        if (_ptr) {
                            _t_fn(index, _ptr);
                        }// end if (ptr)
                        //--------------------------
                        mask &= mask - 1;
                    }// end  while (mask)
                    //--------------------------
                    _hint = _part_opt.value() + 1;
                }// end for
            }// end void for_each_active_fast(std::function<void(IndexType index, T*)>&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M > 0) and (M <= 64), bool> find_data(Fn&& _t_fn) const {
                //--------------------------
                uint64_t mask = m_bitmask.load(std::memory_order_acquire);
                //--------------------------
                while (mask) {
                    //--------------------------
                    const uint8_t index = static_cast<uint8_t>(std::countr_zero(mask));
                    //--------------------------
                    if (index < get_capacity()) {
                        //--------------------------
                        auto _ptr = m_slots[index].load(std::memory_order_acquire);
                        if (_ptr and _t_fn(_ptr)) {
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
            }// end std::enable_if_t<(M > 0) and (M <= 64), bool> find_data(auto&& fn) const
            //--------------------------
            template<uint16_t M = N, typename Fn>
            std::enable_if_t<(M == 0) or (M > 64), bool> find_data(Fn&& _t_fn) const {
                //--------------------------
                for (IndexType part = 0; part < get_mask_count(); ++part) {
                    //--------------------------
                    uint64_t mask           = m_bitmask[part].load(std::memory_order_acquire);
                    const IndexType _c_base = static_cast<IndexType>(part * C_BITS_PER_MASK);
                    //--------------------------
                    while (mask) {
                        //--------------------------
                        const IndexType index = _c_base + static_cast<uint8_t>(std::countr_zero(mask));
                        //--------------------------
                        if (index < get_capacity()) {
                            //--------------------------
                            auto _ptr = m_slots[index].load(std::memory_order_acquire);
                            if (_ptr and _t_fn(_ptr)) {
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
            }// end std::enable_if_t<(M == 0) or (M > 64), bool> find_data(Func&& fn) const
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
                        BitmapTree* _p_tree = tree_ptr();
                        _p_tree->reset_set(plane_index(PartPlane::Available));
                        _p_tree->reset_clear(plane_index(PartPlane::NonEmpty));
                    }
                }// end if constexpr ((N > 0) and (N <= 64))
                //--------------------------
                m_size.store(0UL, std::memory_order_release);
                //--------------------------
            }// end void clear_data(void)
            //--------------------------
            IndexType size_data(void) const {
                return static_cast<IndexType>(m_size.load(std::memory_order_relaxed));
            }// end IndexType size_data(void) const
            //--------------------------------------------------------------
            // Helper functions
            //--------------------------------------------------------------
            bool tree_enabled(void) const noexcept {
                if constexpr (!C_TREE_POSSIBLE) {
                    return false;
                } else {
                    if (!m_use_tree) {
                        return false;
                    }
                    if constexpr (C_TREE_ALWAYS) {
                        return true;
                    } else {
                        return static_cast<bool>(m_available);
                    }
                }
            }// end bool tree_enabled(void) const noexcept
            //--------------------------
            BitmapTree* tree_ptr(void) noexcept {
                if constexpr (!C_TREE_POSSIBLE) {
                    return nullptr;
                } else {
                    if constexpr (C_TREE_ALWAYS) {
                        return &m_available;
                    } else {
                        return m_available.get();
                    }
                }
            }// end BitmapTree* tree_ptr(void) noexcept
            //--------------------------
            BitmapTree* tree_ptr(void) const noexcept {
                if constexpr (!C_TREE_POSSIBLE) {
                    return nullptr;
                } else {
                    if constexpr (C_TREE_ALWAYS) {
                        return &m_available;
                    } else {
                        return m_available.get();
                    }
                }
            }// end BitmapTree* tree_ptr(void) const noexcept
            //--------------------------
            void disable_tree(void) noexcept {
                if constexpr (!C_TREE_POSSIBLE) {
                    return;
                } else {
                    m_use_tree = false;
                    if constexpr (C_TREE_ALWAYS) {
                        m_available = BitmapTree();
                    } else {
                        m_available.reset();
                    }
                }
            }// end void disable_tree(void) noexcept
            //--------------------------
            uint8_t select_free_bit(const uint64_t& mask, const uint8_t& _bit_hint) noexcept {
                //--------------------------
                const uint64_t _c_free = ~mask;
                //--------------------------
                if constexpr (C_ENABLE_ROTATION) {
                    if ((_bit_hint != 0) and (std::popcount(_c_free) >= S_C_ROTATE_THRESHOLD)) {
                        //--------------------------
                        const uint64_t _c_rotated   = std::rotr(_c_free, _bit_hint);
                        const uint8_t _c_bit_offset = static_cast<uint8_t>(std::countr_zero(_c_rotated));
                        uint16_t _bit               = static_cast<uint16_t>(_c_bit_offset + _bit_hint);
                        //--------------------------
                        if (_bit >= C_BITS_PER_MASK) {
                            _bit = static_cast<uint16_t>(_bit - C_BITS_PER_MASK);
                        }// end if (_bit >= C_BITS_PER_MASK)
                        //--------------------------
                        return static_cast<uint8_t>(_bit);
                    }// end if ((bit_hint != 0) and (std::popcount(free) >= C_ROTATE_THRESHOLD))
                } else {
                    static_cast<void>(_bit_hint);
                }// end if constexpr (C_ENABLE_ROTATION)
                return static_cast<uint8_t>(std::countr_zero(_c_free));
            }// end select_free_bit
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), std::optional<size_t>>
            scan_available(const size_t& _start_part, const size_t& _c_mask_count_size, const size_t& _c_available_plane) {
                //--------------------------
                const bool _c_use_tree = tree_enabled();
                BitmapTree* _p_tree    = _c_use_tree ? tree_ptr() : nullptr;
                for (size_t _offset = 0; _offset < _c_mask_count_size; ++_offset) {
                    //--------------------------
                    size_t _probe = _start_part + _offset;
                    //--------------------------
                    if (_probe >= _c_mask_count_size) {
                        _probe -= _c_mask_count_size;
                    }// end if (probe >= mask_count_size)
                    //--------------------------
                    if (m_bitmask[_probe].load(std::memory_order_acquire) != ~0ULL) {
                        if (_c_use_tree) {
                            _p_tree->set(_probe, _c_available_plane);
                        }// end if (_use_tree)
                        return _probe;
                    }// end if (m_bitmask[probe].load(std::memory_order_acquire) != ~0ULL)
                }// end for (size_t offset = 0; offset < mask_count_size; ++offset)
                return std::nullopt;
            }// end std::optional<size_t> scan_available(...)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool>
            refresh_hint(const IndexType& part, const size_t& _c_available_plane) noexcept {
                //--------------------------
                if (!tree_enabled()) {
                    return false;
                }// end if (!tree_enabled)
                //--------------------------
                BitmapTree* _p_tree = tree_ptr();
                _p_tree->clear(static_cast<size_t>(part), _c_available_plane);
                if (m_bitmask[part].load(std::memory_order_acquire) != ~0ULL) {
                    _p_tree->set(static_cast<size_t>(part), _c_available_plane);
                }// end if (m_bitmask[part].load(std::memory_order_acquire) != ~0ULL)
                //--------------------------
                return true;
            }// end bool refresh_hint(const IndexType& part, const size_t& available_plane) noexcept
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool>
            update_on_full(const IndexType& part, const uint64_t& desired, const size_t& _c_available_plane) noexcept {
                if (desired != ~0ULL) {
                    return true;
                }// end if (desired != ~0ULL)
                return refresh_hint(part, _c_available_plane);
            }// end bool update_on_full(const IndexType& part, const uint64_t& desired, const size_t& available_plane) noexcept
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
            }// end bool available_not_full(const IndexType& part, const uint64_t& old, const size_t& available_plane) noexcept
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool> mark_non_empty(IndexType part) noexcept {
                if (!tree_enabled()) {
                    return false;
                }// end if (!tree_enabled)
                return tree_ptr()->set(static_cast<size_t>(part), plane_index(PartPlane::NonEmpty));
            }// end std::enable_if_t<(M == 0) or (M > 64), bool> mark_non_empty(IndexType part) noexcept
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M == 0) or (M > 64), bool> clear_non_empty(IndexType part) const noexcept {
                if (!tree_enabled()) {
                    return false;
                }// end if (!tree_enabled)
                return tree_ptr()->clear(static_cast<size_t>(part), plane_index(PartPlane::NonEmpty));
            }// end std::enable_if_t<(M == 0) or (M > 64), bool> clear_non_empty(IndexType part) const noexcept
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
                        m_size.fetch_add(1, std::memory_order_relaxed);
                        return true;
                    }// end if (m_bitmask.compare_exchange_weak(...))
                }// end while ((mask & bit) == 0)
                //--------------------------
                return false;
                //--------------------------
            }// end std::enable_if_t<(M > 0) and (M <= 64), bool> reacquire_index(const IndexType& index)
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
                        m_size.fetch_add(1, std::memory_order_relaxed);
                        const bool _c_marked = mark_non_empty(part);
                        if (_c_marked) {
                            static_cast<void>(update_on_full(part, desired, plane_index(PartPlane::Available)));
                        }
                        return true;
                    }// end if (m_bitmask[part].compare_exchange_weak(...))
                }// end while ((mask & flag) == 0)
                //--------------------------
                return false;
                //--------------------------
            }// end std::enable_if_t<(M == 0) or (M > 64), bool> reacquire_index(const IndexType& index)
            //--------------------------
            template<uint16_t M = N>
            std::enable_if_t<(M > 64) or (M == 0), bool>
            invalid_bits(const IndexType& capacity, const IndexType& _c_mask_count) {
                //--------------------------
                if (!(capacity and _c_mask_count)) {
                    return false;
                }// end if (!(capacity and mask_count))
                //--------------------------
                const IndexType _c_valid_bits = capacity - static_cast<IndexType>((_c_mask_count - 1) * C_BITS_PER_MASK);
                if (_c_valid_bits < C_BITS_PER_MASK) {
                    //--------------------------
                    const uint64_t _c_valid_mask   = (_c_valid_bits == 0) ? 0ULL : ((1ULL << _c_valid_bits) - 1ULL);
                    const uint64_t _c_invalid_mask = ~_c_valid_mask;
                    //--------------------------
                    m_bitmask[_c_mask_count - 1].fetch_or(_c_invalid_mask, std::memory_order_relaxed);
                    //--------------------------
                }// end if (valid_bits < C_BITS_PER_MASK)
                //--------------------------
                return true;
            }// end bool invalid_bits(const IndexType& capacity, const IndexType& mask_count)
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
            }// end std::enable_if_t<(M > 64), bool> Initialization(void)
            //--------------------------
            bool maybe_initialize_tree(const size_t& leaf_bits) {
                //--------------------------
                if constexpr (!C_TREE_POSSIBLE) {
                    return true;
                } else {
                    if (!m_use_tree) {
                        return true;
                    }// end if (!m_use_tree)
                    //--------------------------
                    return initialize_tree(leaf_bits);
                }
            }// end bool maybe_initialize_tree(const size_t& leaf_bits)
            //--------------------------
            bool initialize_tree(const size_t& leaf_bits) {
                //--------------------------
                if constexpr (!C_TREE_POSSIBLE) {
                    return true;
                } else {
                    if (!leaf_bits) {
                        disable_tree();
                        return false;
                    }// end if (!leaf_bits)
                    //--------------------------
                    if constexpr (!C_TREE_ALWAYS) {
                        if (!m_available) {
                            m_available = std::make_unique<BitmapTree>();
                        }// end if (!m_available)
                    }
                    //--------------------------
                    BitmapTree* _p_tree = tree_ptr();
                    if (!_p_tree or !_p_tree->initialization(leaf_bits, plane_count())) {
                        disable_tree();
                        return false;
                    }// end if (!tree or !tree->initialization(leaf_bits, plane_count()))
                    //--------------------------
                    return _p_tree->reset_set(plane_index(PartPlane::Available)) and _p_tree->reset_clear(plane_index(PartPlane::NonEmpty));
                }
            }// end bool initialize_tree(const size_t& leaf_bits)
            //--------------------------------------------------------------
            // Constexpr / Consteval helpers
            //--------------------------------------------------------------
            constexpr IndexType get_capacity(void) const {
                //--------------------------
                if constexpr ((N == 0) or (N > S_C_ARRAY_LIMIT)) {
                    return m_capacity.load(std::memory_order_relaxed);
                }// end if constexpr ((N == 0) or (N > C_ARRAY_LIMIT))
                //--------------------------
                return N;
                //--------------------------
            }// end constexpr IndexType get_capacity(void) const
            //--------------------------
            constexpr IndexType get_mask_count(void) const {
                //--------------------------
                if constexpr ((N == 0) or (N > S_C_ARRAY_LIMIT)) {
                    return m_mask_count.load(std::memory_order_relaxed);
                }// end if constexpr ((N == 0) or (N > C_ARRAY_LIMIT))
                //--------------------------
                return S_C_MASK_COUNT;
                //--------------------------
            }// end constexpr IndexType get_mask_count(void) const
            //--------------------------
            constexpr IndexType part_index(IndexType index) const noexcept {
                return static_cast<IndexType>(index / C_BITS_PER_MASK);
            }// end constexpr IndexType part_index(IndexType index)
            //--------------------------
            constexpr uint16_t bit_index(IndexType index) const noexcept {
                return static_cast<uint16_t>(index % C_BITS_PER_MASK);
            }// end constexpr uint16_t bit_index(IndexType index)
            //--------------------------
            constexpr size_t bitmask_calculator(size_t capacity) noexcept {
                return (capacity) ? static_cast<size_t>((capacity + C_BITS_PER_MASK - 1) / C_BITS_PER_MASK) : 0UL;
            }// end constexpr size_t bitmask_calculator(size_t capacity)
            //--------------------------
            constexpr size_t bitmask_capacity(size_t capacity) noexcept {
                return std::bit_ceil(capacity);
            }// end constexpr size_t bitmask_capacity(size_t capacity)
            //--------------------------
            constexpr bool use_tree(const size_t& capacity) const noexcept {
                return capacity > static_cast<size_t>(S_C_ARRAY_LIMIT);
            }// end constexpr bool use_tree(const size_t& capacity) const noexcept
            //--------------------------
            constexpr size_t plane_index(PartPlane _plane) const noexcept {
                return static_cast<size_t>(_plane);
            }// end constexpr size_t plane_index(PartPlane plane) const noexcept
            //--------------------------
            constexpr size_t plane_count(void) const noexcept {
                return plane_index(PartPlane::Count);
            }// end constexpr size_t plane_count(void) const noexcept
            //--------------------------
            constexpr uint64_t initial_bitmask(void) const noexcept {
                if constexpr ((N > 0) and (N < C_BITS_PER_MASK)) {
                    return ~((1ULL << N) - 1ULL);
                }// end if constexpr ((N > 0) and (N < C_BITS_PER_MASK))
                return 0ULL;
            }// end constexpr uint64_t initial_bitmask(void) const noexcept
            //--------------------------------------------------------------
        private:
            //--------------------------------------------------------------
            std::atomic<size_t> m_capacity, m_mask_count, m_size;
            //--------------------------
            using BitmaskType = std::conditional_t<(N == 0) or (N > S_C_ARRAY_LIMIT), std::vector<std::atomic<uint64_t>>,
                                    std::conditional_t<(N > C_BITS_PER_MASK) and (N <= S_C_ARRAY_LIMIT ), std::array<std::atomic<uint64_t>, S_C_MASK_COUNT>,
                                    std::atomic<uint64_t>>>;
            //--------------------------
            SlotType m_slots;
            BitmaskType m_bitmask;
            mutable TreeStorage m_available;
            bool m_use_tree;
            const bool m_c_initialize;
        //--------------------------------------------------------------
    };// end class BitmaskTable
    //--------------------------------------------------------------
} // namespace HazardSystem
//--------------------------------------------------------------
