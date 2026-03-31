#include <vector>
#include "sample.hpp"
#include <algorithm>

#define badMacro 1

int globalValue = 0;

void MyNs::loop_thing(int& value) const{
    for (int i=0; i<3; ++i) {
        value+=i;
    }
}

MyNs::MyClass::MyClass() {}

MyNs::MyClass::~MyClass() {}

void MyNs::do_thing(int* ptr) {
    static_cast<void>(ptr);
    int localVar=1;
    int otherVar =2;

    loop_thing(localVar);

    int* rawPtr = nullptr;
    const int value = 3;
    static const int total = 4;
}
