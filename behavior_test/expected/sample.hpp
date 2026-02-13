#pragma once
//--------------------------------------------------------------
// Standard Cpp Libraries
//--------------------------------------------------------------
#include <memory>
#include <vector>

//--------------------------------------------------------------
// User Defined Headers
//--------------------------------------------------------------
#include "sample.hpp"
namespace MyNs {

//--------------------------
// User-defined libraries
//--------------------------
class MyClass {
public:
    MyClass(void);
    ~MyClass(void);
    void do_thing(int* p_ptr);
    static constexpr int S_C_MAX_VALUE = 4;
protected:
    void loop_thing(int& m_value) const;
private:
    int m_value;
    static const int m_s_c_count = 2;
    std::shared_ptr<int> m_sp_shared_ptr;
    std::unique_ptr<int> uniquePtr;
    std::weak_ptr<int> m_wp_weak_ptr;
}; // end class MyClass
} // end namespace MyNs
