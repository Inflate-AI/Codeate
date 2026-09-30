class Shape:
    def area(self):
        return 0


class Circle(Shape):
    def area(self, scale):
        total = self.radius * scale
        print(total)
        return total


def add(a, b):
    return a + b
