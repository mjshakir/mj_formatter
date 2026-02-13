//--------------------------------------------------------------
// Main header
//--------------------------------------------------------------
#include "sample.hpp"

//--------------------------------------------------------------
// Standard Cpp Libraries
//--------------------------------------------------------------
#include <algorithm>
#include <vector>
//--------------------------------------------------------------
// user defined macros
//--------------------------------------------------------------

#define BAD_MACRO 1

//--------------------------------------------------------------
// Global Veriables
//--------------------------------------------------------------
int g_global_value = 0;

//--------------------------------------------------------------
// Class Constructors
//--------------------------------------------------------------
MyNs::MyClass::MyClass(void) {} // end MyNs::MyClass::MyClass(void)

MyNs::MyClass::~MyClass(void) {} // end MyNs::MyClass::~MyClass(void)

//--------------------------------------------------------------
// Public functions
//--------------------------------------------------------------
void MyNs::do_thing(int* p_ptr) {
    static_cast<void>(p_ptr);
    int localVar = 1;
    int otherVar = 2;

    loop_thing(localVar);

    int* rawPtr            = nullptr;
    const int _c_value     = 3;
    static const int total = 4;
} // end void MyNs::do_thing(int* p_ptr)

//--------------------------------------------------------------
// Protected functions
//--------------------------------------------------------------
void MyNs::loop_thing(int& _c_value) const{
    for (int i=0; i<3; ++i) {
        _c_value+=i;
    }
} // end void MyNs::loop_thing(int& _c_value) const
