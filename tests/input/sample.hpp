#pragma once
#include "sample.hpp"
#include <vector>
#include <memory>
namespace MyNs {

//----------    
// User Defined libraries
//----------
class MyClass {
public:
    MyClass();
    ~MyClass();
    void do_thing(int* ptr);
    static constexpr int maxValue = 4;
protected:
    void loop_thing(int& value) const;
private:
    int value;
    static const int count = 2;
    std::shared_ptr<int> sharedPtr;
    std::unique_ptr<int> uniquePtr;
    std::weak_ptr<int> weakPtr;
};
}
