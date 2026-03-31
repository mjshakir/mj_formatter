//--------------------------------------------------------------
// Main header
//--------------------------------------------------------------
#include "sample.hpp"

//--------------------------------------------------------------
// Standard Cpp Libraries
//--------------------------------------------------------------
#include <algorithm>
#include <vector>
#define badMacro 1

int g_global_value = 0;

void MyNs::loop_thing(int& value) const{
    for (int i=0; i<3; ++i) {
        value+=i;
    }
}

MyNs::MyClass::MyClass(void) {}

MyNs::MyClass::~MyClass(void) {}

void MyNs::do_thing(int* ptr) {
    static_cast<void>(ptr);
    int localVar = 1;
    int otherVar = 2;

    loop_thing(localVar);

    int* rawPtr            = nullptr;
    const int value        = 3;
    static const int total = 4;
}
